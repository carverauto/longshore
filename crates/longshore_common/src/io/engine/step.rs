// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scheduling pass (`step`) and engine-resolved ops (`Timeout`, `AwaitWake`, `Cancel`).

use super::*;

impl<W: World> Engine<W> {
    pub(super) fn record_trace(&mut self, c: Completion) {
        let now = self.world.now_ns();
        #[expect(
            clippy::cast_sign_loss,
            reason = "the trace hashes the bits of `result`, negated errnos included"
        )]
        let result_bits = c.result as u32;
        for v in [c.token.0, u64::from(result_bits), now] {
            self.trace = (self.trace ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        }
        if c.more || c.buf.is_some() {
            let buf = c.buf.map_or(u64::MAX, |b| u64::from(b.0));
            for v in [u64::from(c.more), buf] {
                self.trace = (self.trace ^ v).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }

    fn resolve_or_attempt(&mut self, p: &Pending) -> Option<Attempt> {
        match p.op {
            Op::Timeout { .. } => (self.world.now_ns() >= p.deadline)
                .then_some(0)
                .map(Attempt::single),
            Op::AwaitWake => self
                .woken
                .swap(false, Ordering::AcqRel)
                .then_some(0)
                .map(Attempt::single),
            Op::Cancel { .. } => {
                let target = p.cancel_target;
                match self.pending.iter_mut().find(|q| target == Some(q.id)) {
                    Some(q) => {
                        if !q.cancelled {
                            q.cancelled = true;
                            let op = q.op;
                            self.world.on_cancel(&op);
                        }
                        Some(Attempt::single(0))
                    }
                    None => Some(Attempt::single(-ENOENT)),
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
                    return Some(Attempt::single(-EBADF));
                }
                self.world.attempt_completion(
                    &p.op,
                    &mut self.bufs[self.pool_base..],
                    &self.limits,
                    p.world_tag,
                )
            }
            Op::ConnectUnix { sock, .. } => self
                .world
                .attempt_connect_unix(sock, &p.unix_path[..p.unix_len as usize])
                .map(Attempt::single),
            Op::Connect { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::ReadFile { .. }
            | Op::FileOpen { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => self.world.attempt_completion(
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
        self.step_seq += 1;
        let start = self.world.first(n);
        let mut visited = 0;
        let mut skipped = 0;
        let mut i = start;
        while visited < n && !out.is_full() && !self.pending.is_empty() {
            let idx = i % self.pending.len();
            if self.pending[idx].last_delivered_step == self.step_seq {
                skipped += 1;
                if skipped >= self.pending.len() {
                    break;
                }
                i = idx + 1;
                continue;
            }
            skipped = 0;
            visited += 1;
            let p = self.pending[idx];
            if p.chain_id != 0 && !p.chain_active {
                i = idx + 1;
                continue;
            }
            let ready = if p.cancelled || p.chain_aborted {
                if p.attached {
                    self.world.release(&p.op);
                }
                Some(Attempt::single(-ECANCELED))
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
                Some(Attempt { result, more, buf }) => {
                    let c = Completion {
                        token: p.token,
                        result,
                        more,
                        buf,
                    };
                    if more {
                        self.pending[idx].last_delivered_step = self.step_seq;
                        self.record_trace(c);
                        out.push(c);
                        i = idx + 1;
                    } else {
                        let _: Pending = self.pending.swap_remove(idx);
                        self.finish_op(p, result);
                        self.record_trace(c);
                        out.push(c);
                        // `swap_remove` moved another entry into `idx`; visit it next.
                    }
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
                        | Op::ConnectUnix { sock: s, .. }
                        | Op::ConnectUdp { sock: s, .. }
                        | Op::Accept { listener: s, .. }
                        | Op::ArmedAccept { listener: s }
                        | Op::Send { sock: s, .. }
                        | Op::Recv { sock: s, .. }
                        | Op::RecvNow { sock: s, .. }
                        | Op::ArmedRecv { sock: s } => s == sock,
                        Op::Close { .. }
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
                        | Op::FileMkdir { .. } => false,
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
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => {}
        }
        if p.chain_id != 0 {
            let short_rw = match p.op {
                Op::FileRead { len, .. } | Op::FileWrite { len, .. } => {
                    u32::try_from(result).is_ok_and(|done| done < len)
                }
                Op::Connect { .. }
                | Op::ConnectUnix { .. }
                | Op::ConnectUdp { .. }
                | Op::Accept { .. }
                | Op::ArmedAccept { .. }
                | Op::ArmedRecv { .. }
                | Op::Send { .. }
                | Op::Recv { .. }
                | Op::RecvNow { .. }
                | Op::Close { .. }
                | Op::AwaitWake
                | Op::ReadFile { .. }
                | Op::Timeout { .. }
                | Op::Cancel { .. }
                | Op::FileOpen { .. }
                | Op::FileClose { .. }
                | Op::FileSync { .. }
                | Op::FileReserve { .. }
                | Op::FilePunch { .. }
                | Op::FileStatus { .. }
                | Op::FileRename { .. }
                | Op::FileUnlink { .. }
                | Op::FileMkdir { .. } => false,
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
