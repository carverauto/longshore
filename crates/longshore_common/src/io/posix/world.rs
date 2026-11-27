// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! [`World`] for [`Posix`]: what the shared engine asks of the portable
//! backend, readiness from `kqueue` or `epoll` and attempts made at once.

use super::*;

impl World for Posix {
    fn now_ns(&self) -> u64 {
        monotonic_ns()
    }

    fn unix_secs(&self) -> u64 {
        let mut ts = timespec::default();
        let _: i32 = unsafe { clock_gettime(CLOCK_REALTIME, &mut ts) };
        crate::io::clock_secs(ts.tv_sec)
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.files
            .push(CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?);
        Ok(FileId(self.files.len() as u32 - 1))
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.listen_sock(sock, addr, crate::io::ListenOpts::DEFAULT)
    }

    fn listen_with(
        &mut self,
        sock: Sock,
        addr: SocketAddr,
        opts: crate::io::ListenOpts,
    ) -> io::Result<SocketAddr> {
        self.listen_sock(sock, addr, opts)
    }

    fn keep_alive_timing(&mut self, timing: Option<crate::io::KeepAlive>) {
        self.keep_timing = timing;
    }

    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        self.listen_unix_sock(sock, path)
    }

    fn peer_addr(&self, sock: Sock) -> Option<SocketAddr> {
        let s = &self.socks[sock.0 as usize];
        if s.fd < 0 || s.kind != SockKind::Open {
            return None;
        }
        sock::peer_of(s.fd)
    }

    fn end_sends(&mut self, sock: Sock, on: bool) {
        if let Some(slot) = self.socks.get_mut(sock.0 as usize) {
            slot.eor = on;
        }
    }

    fn no_delay(&mut self, on: bool) {
        self.no_delay = on;
    }

    fn keep_alive(&mut self, on: bool) {
        self.keep_alive = on;
    }

    fn give_back(&mut self, id: BufId) {
        self.give_back_ring(id);
    }

    fn waker(&mut self, woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        self.poller.waker(woken)
    }

    fn is_ready(&self, op: &Op) -> bool {
        match *op {
            // Answered at once: this backend has no receive area.
            Op::ArmedRecvZc { .. } => true,
            Op::Connect { sock, .. }
            | Op::ConnectUnix { sock, .. }
            | Op::Send { sock, .. }
            | Op::SendZc { sock, .. } => {
                let s = &self.socks[sock.0 as usize];
                s.fd < 0 || s.write_ready
            }
            Op::Accept { listener, .. } | Op::ArmedAccept { listener } => {
                let s = &self.socks[listener.0 as usize];
                s.fd < 0 || s.read_ready
            }
            Op::Recv { sock, .. } | Op::RecvNow { sock, .. } | Op::ArmedRecv { sock } => {
                let s = &self.socks[sock.0 as usize];
                s.fd < 0 || s.read_ready
            }
            Op::ConnectUdp { .. }
            | Op::Shutdown { .. }
            | Op::Close { .. }
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::AwaitWake
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
            | Op::FileMkdir { .. } => true,
        }
    }

    fn advance(&mut self) {
        self.poller.poll(&mut self.socks);
        #[cfg(feature = "fault-injection")]
        {
            for i in 0..self.stale_len {
                let (sock, write) = self.stale[i];
                if let Some(s) = self.socks.get_mut(sock.0 as usize) {
                    if s.fd >= 0 {
                        if write {
                            s.write_ready = true;
                        } else {
                            s.read_ready = true;
                        }
                    }
                }
            }
            self.stale_len = 0;
        }
    }

    fn first(&mut self, n: usize) -> usize {
        let start = self.cursor % n;
        self.cursor = self.cursor.wrapping_add(1);
        start
    }

    fn hold(&mut self, _op: &Op) -> bool {
        false
    }

    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.attempt_op(op, pool, limits)
    }

    fn attempt_completion(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        _tag: u64,
    ) -> Option<Attempt> {
        match *op {
            Op::ArmedAccept { listener } => self.attempt_accept_armed(listener),
            Op::ArmedRecv { sock } => self.attempt_recv_armed(sock, pool, limits),
            Op::ArmedRecvZc { .. } => Some(Attempt::single(-crate::io::ENOBUFS)),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
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
            | Op::FileMkdir { .. } => self.attempt_op(op, pool, limits).map(Attempt::single),
        }
    }

    fn attempt_connect_unix(&mut self, sock: Sock, path_with_nul: &[u8]) -> Option<i32> {
        self.connect_unix(sock, path_with_nul)
    }

    fn claimed(&self, op: &Op) -> bool {
        matches!(
            op,
            Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. }
                if self.socks[sock.0 as usize].kind == SockKind::Connecting
        )
    }

    fn release(&mut self, op: &Op) {
        if let Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. } = op {
            let s = &mut self.socks[sock.0 as usize];
            if s.kind == SockKind::Connecting && s.fd >= 0 {
                self.poller.before_close(s);
                let _: i32 = unsafe { close(s.fd) };
                s.reset();
            }
        }
    }

    fn closed(&mut self, _sock: Sock) {}

    fn clear_interest(&mut self) {
        for s in self.socks.iter_mut() {
            s.want_read = false;
            s.want_write = false;
        }
    }

    fn add_interest(&mut self, op: &Op) {
        match *op {
            Op::ArmedRecvZc { .. } => {}
            Op::Accept { listener, .. } | Op::ArmedAccept { listener } => {
                let s = &mut self.socks[listener.0 as usize];
                if s.fd >= 0 && !s.read_ready {
                    s.want_read = true;
                }
            }
            Op::Recv { sock, .. } | Op::ArmedRecv { sock } => {
                let s = &mut self.socks[sock.0 as usize];
                if s.fd >= 0 && !s.read_ready {
                    s.want_read = true;
                }
            }
            Op::Connect { sock, .. }
            | Op::ConnectUnix { sock, .. }
            | Op::Send { sock, .. }
            | Op::SendZc { sock, .. } => {
                let s = &mut self.socks[sock.0 as usize];
                if s.fd >= 0 && !s.write_ready {
                    s.want_write = true;
                }
            }
            Op::ConnectUdp { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
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
    }

    fn wait(&mut self, deadline: Option<u64>, _only_timers: bool, _idle: u32) -> io::Result<()> {
        self.poller.wait(&mut self.socks, deadline, monotonic_ns)
    }
}
