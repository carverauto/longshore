// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scheduling pass (`step`) and engine-resolved ops (`Timeout`, `AwaitWake`, `Cancel`).

use super::*;
use crate::io::Completion;

impl<W: World> Engine<W> {
    fn resolve_or_attempt(&mut self, p: &Pending) -> Option<i32> {
        match p.op {
            Op::Timeout { .. } => (self.world.now_ns() >= p.deadline).then_some(0),
            Op::AwaitWake => self.woken.swap(false, Ordering::AcqRel).then_some(0),
            Op::Cancel { .. } => {
                let target = p.cancel_target;
                match self.pending.iter_mut().find(|q| target == Some(q.id)) {
                    Some(q) => {
                        q.cancelled = true;
                        let op = q.op;
                        self.world.on_cancel(&op);
                        Some(0)
                    }
                    None => Some(-ENOENT),
                }
            }
            _ => self.world.attempt(&p.op, &mut self.bufs, &self.limits),
        }
    }

    pub(crate) fn step(&mut self, out: &mut Completions) {
        self.world.advance();
        let n = self.pending.len();
        if n == 0 {
            return;
        }
        let start = self.world.first(n);
        let mut visited = 0;
        let mut i = start;
        while visited < n && !out.is_full() && !self.pending.is_empty() {
            visited += 1;
            let idx = i % self.pending.len();
            let p = self.pending[idx];
            let ready = if p.cancelled {
                if p.attached {
                    self.world.release(&p.op);
                }
                Some(-ECANCELED)
            } else {
                let visit = p.fresh
                    || matches!(p.op, Op::Timeout { .. } | Op::Cancel { .. } | Op::AwaitWake)
                    || self.world.is_ready(&p.op);
                self.pending[idx].fresh = false;
                if !visit {
                    i = idx + 1;
                    continue;
                }
                if self.world.hold(&p.op) {
                    None
                } else {
                    self.resolve_or_attempt(&p)
                }
            };
            match ready {
                Some(result) => {
                    self.pending.swap_remove(idx);
                    if let Op::Close { sock } = p.op {
                        if !p.cancelled {
                            self.world.closed(sock);
                            for q in self.pending.iter_mut() {
                                let on_sock = match q.op {
                                    Op::Connect { sock: s, .. }
                                    | Op::ConnectUdp { sock: s, .. }
                                    | Op::Accept { listener: s, .. }
                                    | Op::Send { sock: s, .. }
                                    | Op::Recv { sock: s, .. }
                                    | Op::RecvNow { sock: s, .. } => s == sock,
                                    Op::Close { .. }
                                    | Op::AwaitWake
                                    | Op::ReadFile { .. }
                                    | Op::Timeout { .. }
                                    | Op::Cancel { .. } => false,
                                };
                                if on_sock {
                                    if q.attached {
                                        self.world.release(&q.op);
                                        q.attached = false;
                                    }
                                    if !q.cancelled {
                                        q.cancelled = true;
                                        let op = q.op;
                                        self.world.on_cancel(&op);
                                    }
                                }
                            }
                        }
                    }
                    let now = self.world.now_ns();
                    for v in [p.token.0, result as u32 as u64, now] {
                        self.trace = (self.trace ^ v).wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    out.push(Completion {
                        token: p.token,
                        result,
                    });
                    // `swap_remove` moved another entry into `idx`; visit it next.
                }
                None => {
                    if self.world.claimed(&p.op) {
                        self.pending[idx].attached = true;
                    }
                    i = idx + 1;
                }
            }
        }
    }
}
