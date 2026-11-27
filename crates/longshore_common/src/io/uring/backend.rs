// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `Io` trait for the io_uring backend: turning operations into submissions.

use super::sockopts::{apply_tcp_options, read_keep_timing, read_tcp_options};
use super::*;
use crate::io::files::check_unix_path;
use crate::io::{ENAMETOOLONG, ENFILE};

impl Io for UringIo {
    fn now_ns(&self) -> u64 {
        let mut ts = timespec::default();
        let _: i32 = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
        crate::io::clock_ns(ts.sec, ts.nsec)
    }

    fn unix_secs(&self) -> u64 {
        let mut ts = timespec::default();
        let _: i32 = unsafe { clock_gettime(CLOCK_REALTIME, &mut ts) };
        crate::io::clock_secs(ts.sec)
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

    fn peer_addr(&self, sock: Sock) -> Option<SocketAddr> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        self.sock_peer[sock.0 as usize]
    }

    fn end_sends(&mut self, sock: Sock, on: bool) {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        self.sock_eor[sock.0 as usize] = on;
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

    fn give_back_piece(&mut self, page: BufId, off: u32, len: u32) {
        self.return_piece(page, off, len);
    }

    fn take_ring_full(&mut self) -> bool {
        core::mem::take(&mut self.cq_full)
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
            if let Err(errno) =
                apply_tcp_options(fd, self.no_delay, self.keep_alive, self.keep_timing)
            {
                let _: i32 = unsafe { close(fd) };
                return Err(io::Error::from_raw_os_error(errno));
            }
        }
        self.last_listen_opts = Some(read_tcp_options(fd));
        self.last_listen_timing = Some(read_keep_timing(fd));
        let r = self.ring.update_file(sock.0, fd);
        let _: i32 = unsafe { close(fd) };
        r.map(|_| bound)
    }

    fn listen_with(
        &mut self,
        sock: Sock,
        addr: SocketAddr,
        opts: crate::io::ListenOpts,
    ) -> io::Result<SocketAddr> {
        if opts == crate::io::ListenOpts::DEFAULT {
            return self.listen(sock, addr);
        }
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        self.sock_stream[sock.0 as usize] = false;
        let fd = super::listen::open(addr, opts)?;
        let bound = super::listen::bound(fd);
        let r = bound.and_then(|bound| {
            if self.no_delay || self.keep_alive {
                apply_tcp_options(fd, self.no_delay, self.keep_alive, self.keep_timing)
                    .map_err(io::Error::from_raw_os_error)?;
            }
            self.last_listen_opts = Some(read_tcp_options(fd));
            self.last_listen_timing = Some(read_keep_timing(fd));
            self.ring.update_file(sock.0, fd).map(|_| bound)
        });
        let _: i32 = unsafe { close(fd) };
        r
    }

    fn keep_alive_timing(&mut self, timing: Option<crate::io::KeepAlive>) {
        self.keep_timing = timing;
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
        if let Err(errno) = self.precheck(&op) {
            self.slots[i as usize].state = State::Free;
            self.ready.push(i, Completion::new(token, errno));
            return Ok(());
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
        let rows = self.gather.free_rows();
        if let Err((bad_idx, errno)) = self.files_table.check_chain(ops, pool, &self.limits, rows) {
            for (idx, &(token, _)) in ops.iter().enumerate() {
                #[expect(clippy::expect_used, reason = "free.len() >= ops.len(), checked above")]
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
            #[expect(clippy::expect_used, reason = "free.len() >= ops.len(), checked above")]
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
        self.begin_tick(wait);
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
            self.cq_full |= self.ring.cq_is_full();
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
        s.reports_taken = false;
        s.row = super::gather::NO_ROW;
    }
}

impl UringIo {
    /// Checks an operation's arguments before it takes a submission entry;
    /// `Err` carries the errno it completes with at once.
    #[inline]
    fn precheck(&mut self, op: &Op) -> Result<(), i32> {
        if op.is_file_op() {
            let pool = &self.bufs[self.pool_base..];
            self.files_table.check_op(op, pool, &self.limits)?;
            if matches!(op, Op::FileWriteSpans { .. }) && !self.gather.has_row() {
                // As many gathered writes in flight as were given room.
                return Err(-ENOBUFS);
            }
        }
        match *op {
            Op::ArmedAccept { listener } => {
                assert!(
                    listener.0 < self.limits.sockets_max,
                    "socket slot out of range"
                );
                if self.limits.accept_range_len == 0 {
                    return Err(-ENFILE);
                }
            }
            Op::ArmedRecv { sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                if self.limits.ring_buffers == 0 {
                    return Err(-ENOBUFS);
                }
            }
            Op::ArmedRecvZc { sock, credit } => self.precheck_recv_zc(sock, credit)?,
            Op::Shutdown { sock, .. } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                // The same answer as the other backends for a slot that
                // holds no connected stream; the kernel would say EBADF.
                if !self.sock_stream[sock.0 as usize] {
                    return Err(-crate::io::ENOTCONN);
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
                let _: &[u8] =
                    check_unix_path(pool, &self.limits, path_buf, path_off, path_len, 108)?;
            }
            Op::Connect { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
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
            | Op::FileWriteSpans { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => {}
        }
        Ok(())
    }
}
