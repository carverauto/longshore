// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `Io` trait for the simulator.

use super::*;

impl Io for SimIo {
    fn now_ns(&self) -> u64 {
        self.now
    }

    fn unix_secs(&self) -> u64 {
        // 2026-01-01T00:00:00Z plus virtual time.
        1_767_225_600 + self.now / 1_000_000_000
    }

    fn buf(&mut self, id: BufId) -> &mut [u8] {
        let (at, len) = self.limits.span(id);
        &mut self.bufs[at..at + len]
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.limits.pair(&mut self.bufs, a, b)
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

    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        if self.pending.len() == self.limits.ops_max as usize {
            return Err(Busy);
        }
        let deadline = match op {
            Op::Timeout { nanos } => self.now + nanos,
            _ => 0,
        };
        let cancel_target = match op {
            Op::Cancel { target } => self
                .pending
                .iter()
                .find(|q| q.token == target && !matches!(q.op, Op::Cancel { .. }))
                .map(|q| q.id),
            _ => None,
        };
        self.next_op_id += 1;
        self.pending.push(Pending {
            token,
            op,
            deadline,
            cancelled: false,
            id: self.next_op_id,
            cancel_target,
            attached: false,
        });
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        let mut stalled = 0;
        loop {
            self.step(out);
            if !wait || !out.is_empty() {
                return Ok(());
            }
            // Nothing completed. If only timers remain, jump to the earliest.
            let earliest = self
                .pending
                .iter()
                .filter(|p| matches!(p.op, Op::Timeout { .. }))
                .map(|p| p.deadline)
                .min();
            if let Some(t) = earliest {
                if self
                    .pending
                    .iter()
                    .all(|p| matches!(p.op, Op::Timeout { .. }))
                {
                    self.now = self.now.max(t.saturating_sub(STEP_NS));
                }
            }
            stalled += 1;
            if stalled > STALL_TICKS && earliest.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "simulation deadlock: operations pending but none can progress",
                ));
            }
        }
    }
}
