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
        if self
            .socks
            .iter()
            .any(|s| s.state == SockState::Listening && s.addr == Some(addr))
        {
            return Err(io::Error::from(io::ErrorKind::AddrInUse));
        }
        let s = &mut self.socks[sock.0 as usize];
        s.state = SockState::Listening;
        s.addr = Some(addr);
        Ok(addr)
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

    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.attempt_op(op, pool, limits)
    }

    fn claimed(&self, op: &Op) -> bool {
        matches!(
            op,
            Op::Connect { sock, .. } if self.socks[sock.0 as usize].state == SockState::Connecting
        )
    }

    fn release(&mut self, op: &Op) {
        if let Op::Connect { sock, .. } = op {
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
