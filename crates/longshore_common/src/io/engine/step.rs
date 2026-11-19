// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scheduling pass (`step`) and engine-resolved ops (`Timeout`, `AwaitWake`, `Cancel`).

use super::*;

impl<W: World> Engine<W> {
    pub(super) fn record_trace(&mut self, token: Token, result: i32) {
        let now = self.world.now_ns();
        for v in [token.0, result as u32 as u64, now] {
            self.trace = (self.trace ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

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
            Op::FileRead { slot, .. }
            | Op::FileWrite { slot, .. }
            | Op::FileSync { slot, .. }
            | Op::FileReserve { slot, .. }
            | Op::FilePunch { slot, .. }
            | Op::FileClose { slot } => {
                if !self.files.has_file(slot) {
                    let earlier_open = self.pending.iter().any(|q| {
                        q.id < p.id && matches!(q.op, Op::FileOpen { slot: s, .. } if s == slot)
                    });
                    if earlier_open {
                        return None;
                    }
                    return Some(-EBADF);
                }
                self.world.attempt_tagged(
                    &p.op,
                    &mut self.bufs[self.pool_base..],
                    &self.limits,
                    p.world_tag,
                )
            }
            _ => self.world.attempt_tagged(
                &p.op,
                &mut self.bufs[self.pool_base..],
                &self.limits,
                p.world_tag,
            ),
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
            if p.chain_id != 0 && !p.chain_active {
                i = idx + 1;
                continue;
            }
            let ready = if p.cancelled || p.chain_aborted {
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
                    self.finish_op(p, result);
                    self.record_trace(p.token, result);
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

    fn finish_op(&mut self, p: Pending, result: i32) {
        match p.op {
            Op::Close { sock } if !p.cancelled => {
                self.world.closed(sock);
                for q in self.pending.iter_mut() {
                    let on_sock = match q.op {
                        Op::Connect { sock: s, .. }
                        | Op::ConnectUdp { sock: s, .. }
                        | Op::Accept { listener: s, .. }
                        | Op::Send { sock: s, .. }
                        | Op::Recv { sock: s, .. }
                        | Op::RecvNow { sock: s, .. } => s == sock,
                        _ => false,
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
            Op::FileOpen { slot, flags, .. } => {
                self.files.on_open_done(slot, result >= 0, flags.direct)
            }
            Op::FileClose { slot } => self.files.on_close_done(slot, result != -ECANCELED),
            _ => {}
        }
        if p.chain_id != 0 {
            let short_rw = match p.op {
                Op::FileRead { len, .. } | Op::FileWrite { len, .. } => {
                    result >= 0 && (result as u32) < len
                }
                _ => false,
            };
            let abort_tail = result < 0 || short_rw;
            for q in self.pending.iter_mut() {
                if q.chain_id == p.chain_id && q.chain_pos > p.chain_pos {
                    if abort_tail {
                        q.chain_aborted = true;
                    }
                    if q.chain_pos == p.chain_pos + 1 {
                        q.chain_active = true;
                        q.fresh = true;
                    }
                }
            }
        }
    }
}
