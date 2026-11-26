// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! [`World`] implementation for [`Virtual`].

use super::*;

/// The most `Faults::reorder` is taken to be, in parts per million.
const REORDER_MOST: u32 = 900_000;

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
        self.listen_with(sock, addr, crate::io::ListenOpts::DEFAULT)
    }

    /// The backlog is not modelled: a connect waits until it is accepted.
    fn listen_with(
        &mut self,
        sock: Sock,
        addr: SocketAddr,
        opts: crate::io::ListenOpts,
    ) -> io::Result<SocketAddr> {
        let target = Some(SimAddr::Ip(addr));
        // An address is shared only when every listener on it asks to.
        if self.socks.iter().any(|s| {
            s.state == SockState::Listening
                && s.addr == target
                && !(s.reuse_port && opts.reuse_port)
        }) {
            return Err(io::Error::from(io::ErrorKind::AddrInUse));
        }
        let s = &mut self.socks[sock.0 as usize];
        s.state = SockState::Listening;
        s.addr = target;
        s.reuse_port = opts.reuse_port;
        Ok(addr)
    }

    fn keep_alive_timing(&mut self, timing: Option<crate::io::KeepAlive>) {
        self.keep_timing = timing;
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

    fn end_sends(&mut self, sock: Sock, on: bool) {
        self.packets.end_sends(sock.0, on);
    }

    fn keep_alive(&mut self, on: bool) {
        self.keep_alive = on;
    }

    fn give_back(&mut self, id: BufId) {
        self.give_back_ring(id);
    }

    fn give_back_piece(
        &mut self,
        page: BufId,
        off: u32,
        len: u32,
        pool: &mut [u8],
        limits: &Limits,
    ) {
        self.give_back_piece_of(page, off, len, pool, limits);
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

    fn linger(&mut self, _op: &Op) -> u64 {
        // One zero-copy send in eight reports once, as a kernel's may. The
        // others keep their buffer for one to six steps, each its own
        // number, so that several on one socket let go in any order.
        if self.rng.below(8) == 0 {
            0
        } else {
            1 + self.rng.below(6)
        }
    }

    fn late(&mut self) -> bool {
        // Nothing is drawn when it is off: existing traces stay as they are.
        // Never certain, or a close would be put off for ever.
        self.rng.chance(self.faults.reorder.min(REORDER_MOST))
    }

    fn new_tick(&mut self) {
        self.ring_full = false;
    }

    fn gathers(&self) -> bool {
        true
    }

    fn ring_was_full(&self) -> bool {
        self.ring_full
    }

    fn on_submit(&mut self, op: &Op) -> u64 {
        if let Op::ArmedRecvZc { sock, .. } = *op {
            // Armed anew: what a full ring silenced is over.
            self.area.leave(sock.0, false);
        }
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
            Op::ArmedRecvZc { sock, credit } => self.attempt_recv_zc(sock, credit, pool, limits),
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
