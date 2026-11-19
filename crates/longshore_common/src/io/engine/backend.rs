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
        &mut self.bufs[self.pool_base + at..self.pool_base + at + len]
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.limits.pair(&mut self.bufs[self.pool_base..], a, b)
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
        if self.pending.len() + self.ready.len() >= self.limits.ops_max as usize {
            return Err(Busy);
        }
        if op.is_file_op() {
            let pool = &self.bufs[self.pool_base..];
            if let Err(errno) = self.files.check_op(&op, pool, &self.limits) {
                self.ready.push(Completion::new(token, errno));
                return Ok(());
            }
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
                    q.token == target
                        && (q.chain_id == 0 || q.chain_active)
                        && !matches!(
                            q.op,
                            Op::Cancel { .. } | Op::Close { .. } | Op::FileClose { .. }
                        )
                })
                .map(|q| q.id),
            _ => None,
        };
        self.next_op_id += 1;
        let world_tag = self.world.on_submit(&op);
        self.pending.push(Pending {
            token,
            op,
            deadline,
            cancelled: false,
            id: self.next_op_id,
            cancel_target,
            attached: false,
            fresh: true,
            chain_id: 0,
            chain_pos: 0,
            chain_active: true,
            chain_aborted: false,
            world_tag,
            last_delivered_step: 0,
        });
        Ok(())
    }

    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), Busy> {
        if ops.is_empty() {
            return Ok(());
        }
        if self.pending.len() + self.ready.len() + ops.len() > self.limits.ops_max as usize {
            return Err(Busy);
        }
        let pool = &self.bufs[self.pool_base..];
        if let Err((bad_idx, errno)) = self.files.check_chain(ops, pool, &self.limits) {
            for (idx, &(token, _)) in ops.iter().enumerate() {
                self.ready.push(Completion::new(
                    token,
                    if idx == bad_idx { errno } else { -ECANCELED },
                ));
            }
            return Ok(());
        }
        self.next_chain_id += 1;
        let chain_id = self.next_chain_id;
        for (pos, &(token, op)) in ops.iter().enumerate() {
            self.next_op_id += 1;
            let world_tag = self.world.on_submit(&op);
            self.pending.push(Pending {
                token,
                op,
                deadline: 0,
                cancelled: false,
                id: self.next_op_id,
                cancel_target: None,
                attached: false,
                fresh: true,
                chain_id,
                chain_pos: pos as u32,
                chain_active: pos == 0,
                chain_aborted: false,
                world_tag,
                last_delivered_step: 0,
            });
        }
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        let mut idle = 0u32;
        loop {
            while !out.is_full() {
                let Some(c) = self.ready.pop() else { break };
                self.record_trace(c);
                out.push(c);
            }
            if !self.ready.is_empty() {
                return Ok(());
            }
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
                if !p.cancelled && p.chain_active && !p.chain_aborted {
                    self.world.add_interest(&p.op);
                }
            }
            idle += 1;
            self.world.wait(earliest, only_timers, idle)?;
        }
    }
}
