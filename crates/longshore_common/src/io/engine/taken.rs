// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The two reports of an [`Op::SendZc`]: its bytes taken, and its buffer
//! let go of, in a world that goes on reading the buffer in between.

use super::*;

impl<W: World> Engine<W> {
    /// A piece an armed zero-copy receive was just given comes off its
    /// credit: with none left its next visit ends it.
    pub(super) fn spend(&mut self, idx: usize, p: &Pending, got: Option<Attempt>) {
        let (Op::ArmedRecvZc { sock, credit }, Some(piece)) = (p.op, got) else {
            return;
        };
        if piece.more {
            let left = credit.saturating_sub(piece.result.unsigned_abs());
            self.pending[idx].op = Op::ArmedRecvZc { sock, credit: left };
        }
    }

    /// What visiting `p` gave: when it is a zero-copy send whose bytes the
    /// world has just taken and still reads, that becomes its first
    /// report, and its last follows when the world has let go.
    pub(super) fn take(
        &mut self,
        idx: usize,
        p: &Pending,
        got: Option<Attempt>,
    ) -> Option<Attempt> {
        let Op::SendZc { .. } = p.op else {
            return got;
        };
        let attempt = got?;
        if attempt.result <= 0 {
            return Some(attempt);
        }
        let steps = self.world.linger(&p.op);
        if steps == 0 {
            return Some(attempt);
        }
        let sum = self.sent_sum(&p.op, attempt.result);
        let held = &mut self.pending[idx];
        held.taken = Some((attempt.result, self.step_seq + steps));
        held.taken_sum = sum;
        Some(Attempt::new(attempt.result, true, None))
    }

    /// The last report of a zero-copy send that took `result` bytes, once
    /// the world no longer reads them: from step `free_from`, or at once
    /// when it was cancelled or its socket closed. It repeats the result.
    pub(super) fn let_go(&self, p: &Pending, result: i32, free_from: u64) -> Option<Attempt> {
        if !p.cancelled && self.step_seq < free_from {
            return None;
        }
        // Whoever wrote these bytes while they were being sent would have
        // sent other bytes on a real card.
        debug_assert!(
            self.sent_sum(&p.op, result) == p.taken_sum,
            "the bytes of a zero-copy send were written before its last completion"
        );
        Some(Attempt::single(result))
    }

    /// A hash of the first `taken` bytes `op` sends. Only a debug build
    /// compares it.
    fn sent_sum(&self, op: &Op, taken: i32) -> u64 {
        let Op::SendZc { buf, off, len, .. } = *op else {
            return 0;
        };
        if !cfg!(debug_assertions) {
            return 0;
        }
        let range = self.limits.range(buf, off, len.min(taken.unsigned_abs()));
        let mut sum = crate::io::trace::START;
        for byte in &self.bufs[self.pool_base..][range] {
            crate::io::trace::fold(&mut sum, &[u64::from(*byte)]);
        }
        sum
    }
}
