// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `Io` trait for the io_uring backend: turning operations into submissions.

use super::*;

impl Io for UringIo {
    fn now_ns(&self) -> u64 {
        let mut ts = timespec::default();
        unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
        ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
    }

    fn unix_secs(&self) -> u64 {
        let mut ts = timespec::default();
        unsafe { clock_gettime(CLOCK_REALTIME, &mut ts) };
        ts.sec as u64
    }

    fn buf(&mut self, id: BufId) -> &mut [u8] {
        let (at, len) = self.limits.span(id);
        &mut self.bufs[at..at + len]
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.limits.pair(&mut self.bufs, a, b)
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.files
            .push(CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?);
        Ok(FileId(self.files.len() as u32 - 1))
    }

    /// When on, a TCP connect that succeeds completes with the connection's
    /// maximum segment size in place of 0.
    ///
    /// The kernel reads TCP options only through a descriptor, and sockets
    /// made by the ring have none. So a measured connect opens its socket
    /// with `socket(2)`, registers it in its slot, reads the size with
    /// `getsockopt(2)` once connected, and closes the descriptor: four
    /// system calls per connection, and none while data moves. A connect
    /// whose socket cannot be opened that way goes through the ring and
    /// completes with 0.
    fn measure_segment_sizes(&mut self, on: bool) {
        self.measure_segments = on;
    }

    fn waker(&mut self) -> Option<std::sync::Arc<dyn crate::io::Wake>> {
        if self.wake_fd < 0 {
            self.wake_fd = unsafe { eventfd(0, EFD_CLOEXEC) };
        }
        // Each handle owns a descriptor of its own for the same eventfd.
        let fd = unsafe { dup(self.wake_fd) };
        (self.wake_fd >= 0 && fd >= 0).then(|| std::sync::Arc::new(EventFd(fd)) as _)
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        let l = TcpListener::bind(addr)?;
        let bound = l.local_addr()?;
        let fd = l.into_raw_fd();
        let r = self.ring.update_file(sock.0, fd);
        // The registered table holds its own reference.
        unsafe { close(fd) };
        r.map(|_| bound)
    }

    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        let i = self.free.pop().ok_or(Busy)?;
        self.slots[i as usize].token = token.0;
        self.slots[i as usize].state = State::InFlight;
        self.slots[i as usize].phase = 0;
        self.slots[i as usize].cancel_requested = false;
        let mut sqe = match op {
            Op::Connect { sock, addr } | Op::ConnectUdp { sock, addr } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let s = &mut self.slots[i as usize];
                s.state = State::Connecting;
                s.phase = PHASE_SOCKET;
                s.sock = sock.0;
                s.probe_fd = -1;
                (s.addr, s.addr_len) = sockaddr_any::from(addr);
                let family = s.addr.family;
                if self.measure_segments && matches!(op, Op::Connect { .. }) {
                    if let Some(sqe) = self.connect_measured(i, sock, family) {
                        self.push(sqe);
                        return Ok(());
                    }
                }
                // Create the socket directly in the fixed slot; connect follows.
                let kind = if matches!(op, Op::ConnectUdp { .. }) {
                    SOCK_DGRAM
                } else {
                    SOCK_STREAM
                };
                io_uring_sqe {
                    opcode: IORING_OP_SOCKET,
                    fd: family as i32,
                    off: kind,
                    file_index: sock.0 + 1,
                    user_data: PHASE_SOCKET,
                    ..Default::default()
                }
            }
            Op::Accept { listener, sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let mut s = self.fixed(listener);
                s.opcode = IORING_OP_ACCEPT;
                s.file_index = sock.0 + 1;
                s
            }
            Op::Send {
                sock,
                buf,
                off,
                len,
                zero_copy,
            } => {
                let mut s = self.fixed(sock);
                s.addr = self.buf_ptr(buf, off, len);
                s.len = len;
                s.op_flags = MSG_NOSIGNAL;
                if zero_copy {
                    s.opcode = IORING_OP_SEND_ZC;
                    s.ioprio = IORING_RECVSEND_FIXED_BUF;
                    s.buf_index = buf.0 as u16;
                } else {
                    s.opcode = IORING_OP_SEND;
                }
                s
            }
            Op::Recv {
                sock,
                buf,
                off,
                len,
            } => {
                let mut s = self.fixed(sock);
                s.opcode = IORING_OP_RECV;
                s.addr = self.buf_ptr(buf, off, len);
                s.len = len;
                s
            }
            Op::RecvNow {
                sock,
                buf,
                off,
                len,
            } => {
                let mut s = self.fixed(sock);
                s.opcode = IORING_OP_RECV;
                s.addr = self.buf_ptr(buf, off, len);
                s.len = len;
                // Never wait: report what has arrived, or -EAGAIN.
                s.op_flags = MSG_DONTWAIT;
                s
            }
            Op::Close { sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                io_uring_sqe {
                    opcode: IORING_OP_CLOSE,
                    file_index: sock.0 + 1,
                    ..Default::default()
                }
            }
            Op::Cancel { target } => {
                // Find the kernel-side identity of the target operation.
                let found = self
                    .slots
                    .iter()
                    .position(|s| s.state != State::Free && s.token == target.0)
                    .filter(|k| *k as u32 != i);
                let ud = match found {
                    // A close is never cancelled: one that does not run leaks
                    // the descriptor or the socket.
                    Some(k)
                        if matches!(
                            self.slots[k].state,
                            State::FileClose | State::ConnectRelease
                        ) =>
                    {
                        u64::MAX
                    }
                    Some(k) => {
                        // The kernel sees this cancel only at the next enter.
                        // An operation of several steps may have moved on by
                        // then and the cancel would miss it, so remember the
                        // request: the next step is not started.
                        self.slots[k].cancel_requested = true;
                        k as u64 | self.slots[k].phase
                    }
                    // Nothing to cancel: an id no operation uses, so the kernel answers -ENOENT.
                    None => u64::MAX,
                };
                io_uring_sqe {
                    opcode: IORING_OP_ASYNC_CANCEL,
                    fd: -1,
                    addr: ud,
                    ..Default::default()
                }
            }
            Op::ReadFile {
                file,
                buf,
                off,
                len,
            } => {
                let addr = self.buf_ptr(buf, off, len);
                let path = self
                    .files
                    .get(file.0 as usize)
                    .expect("file was registered")
                    .as_ptr() as u64;
                let s = &mut self.slots[i as usize];
                (s.state, s.phase, s.file_addr, s.file_len) =
                    (State::FileOpen, PHASE_FILE_OPEN, addr, len);
                io_uring_sqe {
                    opcode: IORING_OP_OPENAT,
                    fd: AT_FDCWD,
                    addr: path,
                    op_flags: O_RDONLY_CLOEXEC,
                    user_data: PHASE_FILE_OPEN,
                    ..Default::default()
                }
            }
            Op::AwaitWake => {
                assert!(self.wake_fd >= 0, "Op::AwaitWake before Io::waker");
                let s = &mut self.slots[i as usize];
                s.state = State::Wake;
                io_uring_sqe {
                    opcode: IORING_OP_READ,
                    fd: self.wake_fd,
                    addr: &s.wake_count as *const u64 as u64,
                    len: 8,
                    ..Default::default()
                }
            }
            Op::Timeout { nanos } => {
                let s = &mut self.slots[i as usize];
                s.state = State::Timer;
                s.ts = timespec {
                    sec: (nanos / 1_000_000_000) as i64,
                    nsec: (nanos % 1_000_000_000) as i64,
                };
                io_uring_sqe {
                    opcode: IORING_OP_TIMEOUT,
                    addr: &s.ts as *const timespec as u64,
                    len: 1,
                    ..Default::default()
                }
            }
        };
        sqe.user_data |= i as u64;
        self.push(sqe);
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        // A zero-copy result CQE does not produce a completion by itself, so
        // keep waiting until something is deliverable.
        loop {
            self.ring
                .enter(if wait && out.is_empty() { 1 } else { 0 })?;
            while !out.is_full() {
                let Some(c) = self.reap() else { break };
                self.on_cqe(c, out);
            }
            if !wait || !out.is_empty() {
                return Ok(());
            }
        }
    }
}
