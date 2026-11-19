// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `Io` trait implementation for `Engine<W>`.

use super::*;
use crate::io::{BufId, Busy, Io};

impl<W: World> Io for Engine<W> {
    fn now_ns(&self) -> u64 {
        self.world.now_ns()
    }

    fn unix_secs(&self) -> u64 {
        self.world.unix_secs()
    }

    fn buf(&mut self, id: BufId) -> &mut [u8] {
        let (at, len) = self.limits.span(id);
        &mut self.bufs[at..at + len]
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.limits.pair(&mut self.bufs, a, b)
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.world.register_file(path)
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.world.listen(sock, addr)
    }

    fn measure_segment_sizes(&mut self, on: bool) {
        self.world.measure_segment_sizes(on);
    }

    fn waker(&mut self) -> Option<Arc<dyn Wake>> {
        self.world.waker(self.woken.clone())
    }

    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        if self.pending.len() == self.limits.ops_max as usize {
            return Err(Busy);
        }
        let deadline = match op {
            Op::Timeout { nanos } => self.world.now_ns() + nanos,
            _ => 0,
        };
        let cancel_target = match op {
            Op::Cancel { target } => self
                .pending
                .iter()
                .find(|q| {
                    q.token == target && !matches!(q.op, Op::Cancel { .. } | Op::Close { .. })
                })
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
            fresh: true,
        });
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        let mut idle = 0u32;
        loop {
            self.step(out);
            if !wait || !out.is_empty() {
                return Ok(());
            }
            let earliest = self
                .pending
                .iter()
                .filter(|p| matches!(p.op, Op::Timeout { .. }))
                .map(|p| p.deadline)
                .min();
            let only_timers = !self.pending.is_empty()
                && self
                    .pending
                    .iter()
                    .all(|p| matches!(p.op, Op::Timeout { .. }));
            self.world.clear_interest();
            for p in &self.pending {
                if !p.cancelled {
                    self.world.add_interest(&p.op);
                }
            }
            idle += 1;
            self.world.wait(earliest, only_timers, idle)?;
        }
    }
}
