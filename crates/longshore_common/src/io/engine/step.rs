// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scheduling pass (`step`) and engine-resolved ops (`Timeout`, `AwaitWake`, `Cancel`).

use super::*;
use crate::io::trace::fold;

impl<W: World> Engine<W> {
    pub(super) fn record_trace(&mut self, c: Completion) {
        let now = self.world.now_ns();
        #[expect(
            clippy::cast_sign_loss,
            reason = "the trace hashes the bits of `result`, negated errnos included"
        )]
        let result_bits = c.result as u32;
        fold(&mut self.trace, &[c.token.0, u64::from(result_bits), now]);
        if c.more || c.buf.is_some() {
            let buf = c.buf.map_or(u64::MAX, |b| u64::from(b.0));
            fold(&mut self.trace, &[u64::from(c.more), buf]);
        }
        if c.off != 0 {
            // A piece inside its page: traces without one stay as they were.
            fold(&mut self.trace, &[u64::from(c.off)]);
        }
    }

    /// Marks the pending operation `target` names as cancelled: 0, or
    /// `-ENOENT` when it is no longer pending.
    fn resolve_cancel(&mut self, target: Option<u64>) -> i32 {
        match self.pending.iter_mut().find(|q| target == Some(q.id)) {
            Some(q) => {
                if !q.cancelled {
                    q.cancelled = true;
                    let op = q.op;
                    self.world.on_cancel(&op);
                }
                0
            }
            None => -ENOENT,
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
            Op::Cancel { .. } => Some(Attempt::single(self.resolve_cancel(p.cancel_target))),
            // Its credit is used up: the receive ends, as at a stream's end.
            Op::ArmedRecvZc { credit: 0, .. } => Some(Attempt::single(0)),
            Op::FileRead { slot, .. }
            | Op::FileWrite { slot, .. }
            | Op::FileWriteSpans { slot, .. }
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
            | Op::ArmedRecvZc { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
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
            if self.reordered(&p) {
                i = idx + 1;
                continue;
            }
            let ready = if let Some((result, free_from)) = p.taken {
                // A zero-copy send whose bytes went: its last report.
                self.let_go(&p, result, free_from)
            } else if p.cancelled || p.chain_aborted {
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
                    let got = self.resolve_or_attempt(&p);
                    self.spend(idx, &p, got);
                    self.take(idx, &p, got)
                }
            };
            if self.deliver(idx, p, ready, out) {
                i = idx + 1;
            }
            // Otherwise `swap_remove` moved another entry into `idx`; visit it next.
        }
    }

    /// Whether the report of pending `idx` is put off to a later step, for
    /// a world that reorders: the end of a socket operation its close
    /// cancelled, or a close, which then ends its socket's operations now
    /// and completes later.
    fn reordered(&mut self, p: &Pending) -> bool {
        if p.chain_id != 0 {
            return false;
        }
        if let Op::Close { sock } = p.op {
            let late = !p.cancelled && self.world.late();
            if late {
                self.cancel_pending_on(sock);
            }
            return late;
        }
        // An operation cancelled by name reports as promptly as ever.
        p.closed_under && self.world.late()
    }

    /// Delivers the outcome of visiting pending `idx`. Returns whether the
    /// scan moves past `idx`: false when the operation finished and
    /// `swap_remove` moved another entry into its place.
    fn deliver(
        &mut self,
        idx: usize,
        p: Pending,
        ready: Option<Attempt>,
        out: &mut Completions,
    ) -> bool {
        let Some(Attempt {
            result,
            more,
            buf,
            off,
        }) = ready
        else {
            if self.world.claimed(&p.op) {
                self.pending[idx].attached = true;
            }
            return true;
        };
        let c = Completion {
            token: p.token,
            result,
            more,
            buf,
            off,
        };
        if more {
            self.pending[idx].last_delivered_step = self.step_seq;
            self.record_trace(c);
            out.push(c);
            return true;
        }
        let _: Pending = self.pending.swap_remove(idx);
        self.finish_op(p, result);
        self.record_trace(c);
        out.push(c);
        false
    }

    fn finish_op(&mut self, p: Pending, result: i32) {
        match p.op {
            Op::Close { sock } if !p.cancelled => self.cancel_ops_on(sock),
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
            | Op::ArmedRecvZc { .. }
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
        if p.chain_id != 0 {
            self.advance_chain(p, result);
        }
    }

    /// A completed close cancels every operation still pending on its socket.
    fn cancel_ops_on(&mut self, sock: Sock) {
        self.world.closed(sock);
        self.cancel_pending_on(sock);
    }

    /// Cancel every operation pending on `sock`. (A close is not one of
    /// them: it names its socket, it does not run on it.)
    fn cancel_pending_on(&mut self, sock: Sock) {
        for q in self.pending.iter_mut() {
            if op_socket(&q.op) != Some(sock) {
                continue;
            }
            if q.attached {
                self.world.release(&q.op);
                q.attached = false;
            }
            if !q.cancelled {
                (q.cancelled, q.closed_under) = (true, true);
                let op = q.op;
                self.world.on_cancel(&op);
            }
        }
    }

    /// Activates the next link of `p`'s chain, aborting the rest of the
    /// chain when `p` failed or read or wrote short.
    fn advance_chain(&mut self, p: Pending, result: i32) {
        let abort_tail = result < 0 || is_short_rw(&p.op, result);
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

/// The socket a socket operation runs on; `None` for a close and every
/// non-socket operation.
fn op_socket(op: &Op) -> Option<Sock> {
    match *op {
        Op::Connect { sock: s, .. }
        | Op::ConnectUnix { sock: s, .. }
        | Op::ConnectUdp { sock: s, .. }
        | Op::Accept { listener: s, .. }
        | Op::ArmedAccept { listener: s }
        | Op::Send { sock: s, .. }
        | Op::SendZc { sock: s, .. }
        | Op::Recv { sock: s, .. }
        | Op::RecvNow { sock: s, .. }
        | Op::Shutdown { sock: s, .. }
        | Op::ArmedRecv { sock: s }
        | Op::ArmedRecvZc { sock: s, .. } => Some(s),
        Op::Close { .. }
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
        | Op::FileMkdir { .. } => None,
    }
}

/// Whether a file read or write moved fewer bytes than it asked for.
fn is_short_rw(op: &Op, result: i32) -> bool {
    match *op {
        Op::FileRead { len, .. } | Op::FileWrite { len, .. } | Op::FileWriteSpans { len, .. } => {
            u32::try_from(result).is_ok_and(|done| done < len)
        }
        Op::Connect { .. }
        | Op::ConnectUnix { .. }
        | Op::ConnectUdp { .. }
        | Op::Accept { .. }
        | Op::ArmedAccept { .. }
        | Op::ArmedRecv { .. }
        | Op::ArmedRecvZc { .. }
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
        | Op::FileSync { .. }
        | Op::FileReserve { .. }
        | Op::FilePunch { .. }
        | Op::FileStatus { .. }
        | Op::FileRename { .. }
        | Op::FileUnlink { .. }
        | Op::FileMkdir { .. } => false,
    }
}
