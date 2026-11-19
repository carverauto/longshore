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

    fn waker(&mut self) -> Option<std::sync::Arc<dyn crate::io::Wake>> {
        if self.wake_fd < 0 {
            self.wake_fd = unsafe { eventfd(0, EFD_CLOEXEC) };
        }
        let fd = unsafe { dup(self.wake_fd) };
        (self.wake_fd >= 0 && fd >= 0).then(|| std::sync::Arc::new(EventFd(fd)) as _)
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.sock_stream[sock.0 as usize] = false;
        let l = TcpListener::bind(addr)?;
        let bound = l.local_addr()?;
        let fd = l.into_raw_fd();
        let r = self.ring.update_file(sock.0, fd);
        unsafe { close(fd) };
        r.map(|_| bound)
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
        if let Some(mut sqe) = self.prepare_sqe(i, op) {
            sqe.user_data |= i as u64;
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
            if let Some(mut sqe) = self.prepare_sqe(i, op) {
                sqe.user_data |= i as u64;
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
}
