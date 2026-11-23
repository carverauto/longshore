// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `Io` trait for the io_uring backend: turning operations into submissions.

use super::*;
use crate::io::files::check_unix_path;
use crate::io::{EINVAL, ENAMETOOLONG, ENFILE};
use core::ffi::c_int;

impl Io for UringIo {
    fn now_ns(&self) -> u64 {
        let mut ts = timespec::default();
        let _: i32 = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
        ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
    }

    fn unix_secs(&self) -> u64 {
        let mut ts = timespec::default();
        let _: i32 = unsafe { clock_gettime(CLOCK_REALTIME, &mut ts) };
        ts.sec as u64
    }

    fn buf(&mut self, id: BufId) -> &mut [u8] {
        let (at, len) = self.limits.span(id);
        &mut self.bufs[self.pool_base + at..self.pool_base + at + len]
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.limits.pair(&mut self.bufs[self.pool_base..], a, b)
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.files
            .push(CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?);
        Ok(FileId(self.files.len() as u32 - 1))
    }

    /// When on, a TCP connect that succeeds completes with the connection's
    /// maximum segment size in place of 0.
    fn measure_segment_sizes(&mut self, on: bool) {
        self.measure_segments = on;
    }

    fn no_delay(&mut self, on: bool) {
        self.no_delay = on;
    }

    fn keep_alive(&mut self, on: bool) {
        self.keep_alive = on;
    }

    fn give_back(&mut self, id: BufId) {
        assert!(
            self.limits.is_ring_buf(id),
            "give_back requires a ring buffer"
        );
        let ring_id = (id.0 - self.limits.ring_base()) as usize;
        assert!(!self.ring_in_ring[ring_id], "ring buffer already in ring");
        self.return_ring_buf(ring_id as u16);
    }

    fn waker(&mut self) -> Option<std::sync::Arc<dyn crate::io::Wake>> {
        if self.wake_fd < 0 {
            self.wake_fd = unsafe { eventfd(0, EFD_CLOEXEC) };
        }
        let fd = unsafe { dup(self.wake_fd) };
        (self.wake_fd >= 0 && fd >= 0).then(|| std::sync::Arc::new(EventFd(fd)) as _)
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        self.sock_stream[sock.0 as usize] = false;
        let l = TcpListener::bind(addr)?;
        let bound = l.local_addr()?;
        let fd = l.into_raw_fd();
        if self.no_delay || self.keep_alive {
            if let Err(errno) = apply_tcp_options(fd, self.no_delay, self.keep_alive) {
                let _: i32 = unsafe { close(fd) };
                return Err(io::Error::from_raw_os_error(errno));
            }
        }
        self.last_listen_opts = Some(read_tcp_options(fd));
        let r = self.ring.update_file(sock.0, fd);
        let _: i32 = unsafe { close(fd) };
        r.map(|_| bound)
    }

    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        if path.is_empty() || path.as_bytes().contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if path.len() >= 108 {
            return Err(io::Error::from_raw_os_error(ENAMETOOLONG));
        }
        self.sock_stream[sock.0 as usize] = false;
        let l = std::os::unix::net::UnixListener::bind(path)?;
        let fd = l.into_raw_fd();
        let r = self.ring.update_file(sock.0, fd);
        let _: i32 = unsafe { close(fd) };
        r
    }

    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        let i = self.free.pop().ok_or(Busy)?;
        self.init_slot(i, token);
        if op.is_file_op() {
            let pool = &self.bufs[self.pool_base..];
            if let Err(errno) = self.files_table.check_op(&op, pool, &self.limits) {
                self.slots[i as usize].state = State::Free;
                self.ready.push(i, Completion::new(token, errno));
                return Ok(());
            }
        }
        match op {
            Op::ArmedAccept { listener } => {
                assert!(
                    listener.0 < self.limits.sockets_max,
                    "socket slot out of range"
                );
                if self.limits.accept_range_len == 0 {
                    self.slots[i as usize].state = State::Free;
                    self.ready.push(i, Completion::new(token, -ENFILE));
                    return Ok(());
                }
            }
            Op::ArmedRecv { sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                if self.limits.ring_buffers == 0 {
                    self.slots[i as usize].state = State::Free;
                    self.ready.push(i, Completion::new(token, -ENOBUFS));
                    return Ok(());
                }
            }
            Op::ConnectUnix {
                sock,
                path_buf,
                path_off,
                path_len,
            } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let pool = &self.bufs[self.pool_base..];
                if let Err(errno) =
                    check_unix_path(pool, &self.limits, path_buf, path_off, path_len, 108)
                {
                    self.slots[i as usize].state = State::Free;
                    self.ready.push(i, Completion::new(token, errno));
                    return Ok(());
                }
            }
            Op::Connect { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => {}
        }
        if let Some(mut sqe) = self.prepare_sqe(i, op, token) {
            sqe.user_data |= u64::from(i);
            self.push(sqe);
        }
        Ok(())
    }

    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), Busy> {
        if ops.is_empty() {
            return Ok(());
        }
        if self.free.len() < ops.len() {
            return Err(Busy);
        }
        let pool = &self.bufs[self.pool_base..];
        if let Err((bad_idx, errno)) = self.files_table.check_chain(ops, pool, &self.limits) {
            for (idx, &(token, _)) in ops.iter().enumerate() {
                let i = self.free.pop().expect("checked free.len()");
                self.slots[i as usize].state = State::Free;
                self.ready.push(
                    i,
                    Completion::new(token, if idx == bad_idx { errno } else { -ECANCELED }),
                );
            }
            return Ok(());
        }
        let last = ops.len() - 1;
        for (idx, &(token, op)) in ops.iter().enumerate() {
            let i = self.free.pop().expect("checked free.len()");
            self.init_slot(i, token);
            if let Some(mut sqe) = self.prepare_sqe(i, op, token) {
                sqe.user_data |= u64::from(i);
                if idx < last {
                    sqe.flags |= IOSQE_IO_LINK;
                }
                self.push(sqe);
            }
        }
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        #[cfg(feature = "fault-injection")]
        if core::mem::take(&mut self.fail_tick) {
            return Err(io::Error::other("injected: tick fails"));
        }
        loop {
            while !out.is_full() {
                let Some((slot_i, c)) = self.ready.pop() else {
                    break;
                };
                self.slots[slot_i as usize].state = State::Free;
                self.free.push(slot_i);
                out.push(c);
            }
            if !self.ready.is_empty() {
                return Ok(());
            }
            if self.ring.pending() == 0 {
                while !out.is_full() {
                    let Some(c) = self.reap() else { break };
                    self.on_cqe(c, out);
                }
                if !out.is_empty() {
                    return Ok(());
                }
            }
            let min_complete = if wait && out.is_empty() { 1 } else { 0 };
            let to_submit = self.ring.pending();
            let consumed = self.ring.enter(min_complete)?;
            if consumed != to_submit {
                return Err(io::Error::other(format!(
                    "io_uring_enter consumed {consumed} of {to_submit} queued submissions"
                )));
            }
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

impl UringIo {
    fn init_slot(&mut self, i: u32, token: Token) {
        let s = &mut self.slots[i as usize];
        s.token = token.0;
        s.state = State::InFlight;
        s.is_stream = false;
        s.phase = 0;
        s.op_sock = None;
        s.sock_closed = false;
        s.cancel_requested = false;
    }

    /// `(TCP_NODELAY, SO_KEEPALIVE)` read from the most recent `listen` descriptor.
    pub fn last_listen_tcp_options(&self) -> Option<(bool, bool)> {
        self.last_listen_opts
    }

    /// `(TCP_NODELAY, SO_KEEPALIVE)` read from the most recent descriptor-created `Connect`.
    pub fn last_connect_tcp_options(&self) -> Option<(bool, bool)> {
        self.last_connect_opts
    }

    /// Read `(TCP_NODELAY, SO_KEEPALIVE)` directly from fixed-file slot `sock`
    /// using `SOCKET_URING_OP_GETSOCKOPT` (Linux 6.7+). Returns `Ok(None)` on
    /// kernels before 6.7 (`-EINVAL` / `-EOPNOTSUPP`). Requires `in_flight == 0`.
    pub fn probe_tcp_options(&mut self, sock: Sock) -> io::Result<Option<(bool, bool)>> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        assert_eq!(self.in_flight, 0, "probe_tcp_options requires an idle ring");
        let probe_one =
            |ring: &mut Ring, level: c_int, optname: c_int| -> io::Result<Option<bool>> {
                let mut val: c_int = 0;
                let sqe = io_uring_sqe {
                    opcode: IORING_OP_URING_CMD,
                    flags: IOSQE_FIXED_FILE,
                    fd: sock.0 as i32,
                    off: SOCKET_URING_OP_GETSOCKOPT,
                    addr: u64::from(level as u32) | (u64::from(optname as u32) << 32),
                    file_index: core::mem::size_of::<c_int>() as u32,
                    addr3: (&mut val as *mut c_int) as u64,
                    ..Default::default()
                };
                ring.push(sqe).map_err(|_| io::Error::other("sq full"))?;
                let _: u32 = ring.enter(1)?;
                let cqe = ring.pop().ok_or_else(|| io::Error::other("missing cqe"))?;
                if cqe.res == -EINVAL || cqe.res == -95 {
                    return Ok(None);
                }
                if cqe.res < 0 {
                    return Err(io::Error::from_raw_os_error(-cqe.res));
                }
                Ok(Some(val != 0))
            };
        let Some(nd) = probe_one(&mut self.ring, IPPROTO_TCP, TCP_NODELAY)? else {
            return Ok(None);
        };
        let Some(ka) = probe_one(&mut self.ring, SOL_SOCKET, SO_KEEPALIVE)? else {
            return Ok(None);
        };
        Ok(Some((nd, ka)))
    }
}

pub(super) fn apply_tcp_options(fd: c_int, no_delay: bool, keep_alive: bool) -> Result<(), i32> {
    let on: c_int = 1;
    let len = core::mem::size_of::<c_int>() as u32;
    if no_delay {
        let r = unsafe {
            setsockopt(
                fd,
                IPPROTO_TCP,
                TCP_NODELAY,
                (&on as *const c_int).cast(),
                len,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error().raw_os_error().unwrap_or(EINVAL));
        }
    }
    if keep_alive {
        let r = unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_KEEPALIVE,
                (&on as *const c_int).cast(),
                len,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error().raw_os_error().unwrap_or(EINVAL));
        }
    }
    Ok(())
}

pub(super) fn read_tcp_options(fd: c_int) -> (bool, bool) {
    let read_one = |level: c_int, name: c_int| -> bool {
        let mut val: c_int = 0;
        let mut len = core::mem::size_of::<c_int>() as u32;
        let r = unsafe { getsockopt(fd, level, name, (&mut val as *mut c_int).cast(), &mut len) };
        r == 0 && val != 0
    };
    (
        read_one(IPPROTO_TCP, TCP_NODELAY),
        read_one(SOL_SOCKET, SO_KEEPALIVE),
    )
}
