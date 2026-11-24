// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! [`World`] implementation for [`Virtual`].

use super::*;

impl World for Virtual {
    fn now_ns(&self) -> u64 {
        self.now
    }

    fn unix_secs(&self) -> u64 {
        // 2026-01-01T00:00:00Z plus virtual time.
        1_767_225_600 + self.now / 1_000_000_000
    }

    fn register_file(&mut self, _path: &str) -> io::Result<FileId> {
        self.files.push(None);
        Ok(FileId(self.files.len() as u32 - 1))
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        let target = Some(SimAddr::Ip(addr));
        if self
            .socks
            .iter()
            .any(|s| s.state == SockState::Listening && s.addr == target)
        {
            return Err(io::Error::from(io::ErrorKind::AddrInUse));
        }
        let s = &mut self.socks[sock.0 as usize];
        s.state = SockState::Listening;
        s.addr = target;
        Ok(addr)
    }

    fn peer_addr(&self, sock: Sock) -> Option<SocketAddr> {
        let s = &self.socks[sock.0 as usize];
        (s.state == SockState::Open)
            .then_some(s.accepted_from)
            .flatten()
    }

    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        if path.is_empty() || path.as_bytes().contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if path.len() >= 108 {
            return Err(io::Error::from_raw_os_error(ENAMETOOLONG));
        }
        let mut buf = [0u8; 108];
        buf[..path.len()].copy_from_slice(path.as_bytes());
        let target = Some(SimAddr::Unix(buf, (path.len() + 1) as u8));
        if self
            .socks
            .iter()
            .any(|s| s.state == SockState::Listening && s.addr == target)
        {
            return Err(io::Error::from(io::ErrorKind::AddrInUse));
        }
        let s = &mut self.socks[sock.0 as usize];
        s.state = SockState::Listening;
        s.addr = target;
        Ok(())
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
        Some(Arc::new(SimWaker(woken)))
    }

    fn is_ready(&self, _op: &Op) -> bool {
        true
    }

    fn advance(&mut self) {
        self.now += STEP_NS;
    }

    fn first(&mut self, n: usize) -> usize {
        self.rng.below(n as u64) as usize
    }

    fn hold(&mut self, _op: &Op) -> bool {
        self.rng.chance(self.faults.delay)
    }

    fn on_submit(&mut self, op: &Op) -> u64 {
        if op.is_file_op() {
            self.file_op_seq += 1;
            self.file_op_seq
        } else {
            0
        }
    }

    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.attempt_op(op, pool, limits, 0)
    }

    fn attempt_tagged(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        tag: u64,
    ) -> Option<i32> {
        self.attempt_op(op, pool, limits, tag)
    }

    fn attempt_completion(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        tag: u64,
    ) -> Option<Attempt> {
        match *op {
            Op::ArmedAccept { listener } => self.attempt_accept_armed(listener),
            Op::ArmedRecv { sock } => self.attempt_recv_armed(sock, pool, limits),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
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
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => self.attempt_op(op, pool, limits, tag).map(Attempt::single),
        }
    }

    fn attempt_connect_unix(&mut self, sock: Sock, path_with_nul: &[u8]) -> Option<i32> {
        self.connect_unix(sock, path_with_nul)
    }

    fn claimed(&self, op: &Op) -> bool {
        matches!(
            op,
            Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. }
                if self.socks[sock.0 as usize].state == SockState::Connecting
        )
    }

    fn release(&mut self, op: &Op) {
        if let Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. } = op {
            if self.socks[sock.0 as usize].state == SockState::Connecting {
                self.socks[sock.0 as usize].state = SockState::Free;
            }
        }
    }

    fn closed(&mut self, _sock: Sock) {}

    fn wait(&mut self, deadline: Option<u64>, only_timers: bool, idle: u32) -> io::Result<()> {
        if let Some(t) = deadline {
            if only_timers {
                self.now = self.now.max(t.saturating_sub(STEP_NS));
            }
        }
        if idle > STALL_TICKS && deadline.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "simulation deadlock: operations pending but none can progress",
            ));
        }
        Ok(())
    }
}
