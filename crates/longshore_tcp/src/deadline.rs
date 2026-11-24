// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A fixed table of deadlines, one per slot.
//!
//! Each connection and listener has at most one deadline at a time (its
//! connect, handshake or idle timeout, or its next reconnect or accept
//! attempt), so the table has one entry per slot and never grows. The
//! earliest is remembered and found again only when it was the one changed.

const NONE: u64 = u64::MAX;

/// One deadline per slot, in the caller's nanoseconds.
pub struct Deadlines {
    at: Box<[u64]>,
    /// The earliest deadline, when `fresh`.
    earliest: u64,
    fresh: bool,
}

impl Deadlines {
    /// No deadline in any of `slots` slots. Allocates once, here.
    pub fn new(slots: u32) -> Deadlines {
        Deadlines {
            at: vec![NONE; slots as usize].into_boxed_slice(),
            earliest: NONE,
            fresh: true,
        }
    }

    /// Set slot `slot`'s deadline, replacing any it had.
    #[inline]
    pub fn set(&mut self, slot: u32, at_ns: u64) {
        let at_ns = at_ns.min(NONE - 1);
        let old = core::mem::replace(&mut self.at[slot as usize], at_ns);
        if at_ns <= self.earliest {
            // Still the earliest known, whether or not the cache was fresh.
            if self.fresh || at_ns < self.earliest {
                self.earliest = at_ns;
            }
        } else if old == self.earliest {
            self.fresh = false;
        }
    }

    /// Remove slot `slot`'s deadline.
    #[inline]
    pub fn clear(&mut self, slot: u32) {
        let old = core::mem::replace(&mut self.at[slot as usize], NONE);
        if old != NONE && old == self.earliest {
            self.fresh = false;
        }
    }

    /// Slot `slot`'s deadline, if it has one.
    #[inline]
    pub fn get(&self, slot: u32) -> Option<u64> {
        let at = self.at[slot as usize];
        (at != NONE).then_some(at)
    }

    /// The earliest deadline of all, if there is one.
    pub fn earliest(&mut self) -> Option<u64> {
        if !self.fresh {
            self.earliest = self.at.iter().copied().min().unwrap_or(NONE);
            self.fresh = true;
        }
        (self.earliest != NONE).then_some(self.earliest)
    }

    /// A slot whose deadline is at or before `now_ns`, its deadline
    /// removed; `None` when none is due. Costs nothing in that case.
    pub fn next_due(&mut self, now_ns: u64) -> Option<u32> {
        if self.earliest().is_none_or(|e| e > now_ns) {
            return None;
        }
        let slot = self.at.iter().position(|at| *at <= now_ns)?;
        self.at[slot] = NONE;
        self.fresh = false;
        Some(slot as u32)
    }

    /// Hand each slot whose deadline is at or before `now_ns` to `f`, in
    /// slot order, removing its deadline first (`f` may set a new one
    /// through its own means afterwards). Costs nothing when none is due.
    pub fn expire(&mut self, now_ns: u64, mut f: impl FnMut(u32)) {
        if self.earliest().is_none_or(|e| e > now_ns) {
            return;
        }
        for slot in 0..self.at.len() {
            if self.at[slot] <= now_ns {
                self.at[slot] = NONE;
                f(slot as u32);
            }
        }
        self.fresh = false;
    }
}
