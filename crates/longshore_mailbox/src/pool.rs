// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Generic slot pools with preallocated waiter queues that grow dynamically
//! if exceeded, plus single- and multi-pool waiting helpers.

use crate::sync::Mutex;
use core::task::{Context, Poll, Waker};
use std::collections::VecDeque;

/// A fresh name for a caller that may wait on a [`Pool`].
pub fn waiter_id() -> u64 {
    // Only a source of distinct numbers, so never the model checker's kind.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Indices that callers take and give back, waiting when none is free.
pub struct Pool {
    slots: u32,
    state: Mutex<PoolState>,
}

struct PoolState {
    free: Vec<u32>,
    waiters: VecDeque<(u64, Waker)>,
}

impl PoolState {
    fn pass_on(&mut self) {
        if !self.free.is_empty() {
            if let Some((_, w)) = self.waiters.pop_front() {
                w.wake();
            }
        }
    }
}

impl Pool {
    /// Create a pool with `slots` indices (`0..slots`) and preallocated space
    /// for `waiters_prealloc` concurrent waiters.
    pub fn new(slots: u32, waiters_prealloc: usize) -> Self {
        assert!(slots >= 1, "pool requires at least one slot");
        let pool = Self {
            slots,
            state: Mutex::new(PoolState {
                free: (0..slots).rev().collect(),
                waiters: VecDeque::with_capacity(waiters_prealloc),
            }),
        };
        #[cfg(not(longshore_model))]
        drop(pool.state.lock());
        pool
    }

    /// Total number of slots in this pool.
    pub fn slots(&self) -> u32 {
        self.slots
    }

    /// Take a free slot, or register `id` to be woken when one is returned.
    /// Re-polling updates the stored waker in place.
    pub fn poll_take(&self, id: u64, cx: &mut Context<'_>) -> Poll<u32> {
        let mut s = self.state.lock().expect("pool lock");
        if let Some(slot) = s.free.pop() {
            s.waiters.retain(|(w, _)| *w != id);
            return Poll::Ready(slot);
        }
        match s.waiters.iter_mut().find(|(w, _)| *w == id) {
            Some((_, waker)) => waker.clone_from(cx.waker()),
            None => s.waiters.push_back((id, cx.waker().clone())),
        }
        Poll::Pending
    }

    /// Remove `id` from the waiter queue, passing its wake on if a slot is
    /// already free.
    pub fn forget(&self, id: u64) {
        let mut s = self.state.lock().expect("pool lock");
        s.waiters.retain(|(w, _)| *w != id);
        s.pass_on();
    }

    /// Wake every currently queued waiter.
    pub fn wake_all(&self) {
        let mut s = self.state.lock().expect("pool lock");
        while let Some((_, w)) = s.waiters.pop_front() {
            w.wake();
        }
    }

    /// Return `slot` to the pool and wake the longest-waiting caller.
    pub fn give(&self, slot: u32) {
        let mut s = self.state.lock().expect("pool lock");
        s.free.push(slot);
        s.pass_on();
    }
}

/// Caller state for waiting on one [`Pool`] or across several [`Pool`]s.
pub struct SlotWaiter {
    id: u64,
    queued: bool,
}

impl Default for SlotWaiter {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotWaiter {
    pub fn new() -> Self {
        Self {
            id: waiter_id(),
            queued: false,
        }
    }

    /// Poll a single open pool for a slot, returning `Poll::Ready(None)` if
    /// `is_open()` becomes false.
    pub fn poll_one(
        &mut self,
        pool: &Pool,
        is_open: impl Fn() -> bool,
        cx: &mut Context<'_>,
    ) -> Poll<Option<u32>> {
        loop {
            if !is_open() {
                self.forget_one(pool);
                return Poll::Ready(None);
            }
            match pool.poll_take(self.id, cx) {
                Poll::Ready(slot) => {
                    self.queued = false;
                    if is_open() {
                        return Poll::Ready(Some(slot));
                    }
                    pool.give(slot);
                }
                Poll::Pending => {
                    self.queued = true;
                    if is_open() {
                        return Poll::Pending;
                    }
                }
            }
        }
    }

    /// Poll across `entries` starting at `first`, skipping closed entries
    /// unless `all_closed && take_closed`. Returns `Poll::Ready(None)` if no
    /// eligible entry has a slot when all entries are closed.
    pub fn poll_many<T>(
        &mut self,
        entries: &[T],
        first: usize,
        pool_of: impl Fn(&T) -> &Pool,
        is_open: impl Fn(&T) -> bool,
        take_closed: bool,
        cx: &mut Context<'_>,
    ) -> Poll<Option<(usize, u32)>> {
        let count = entries.len();
        if count == 0 {
            return Poll::Ready(None);
        }
        if count == 1 && !take_closed {
            return self
                .poll_one(pool_of(&entries[0]), || is_open(&entries[0]), cx)
                .map(|slot| slot.map(|s| (0, s)));
        }
        loop {
            let closed = entries.iter().filter(|e| !is_open(e)).count();
            let all_closed = closed == count;
            if all_closed && !take_closed {
                self.forget_many(entries, &pool_of);
                return Poll::Ready(None);
            }
            for n in 0..count {
                let idx = (first + n) % count;
                let entry = &entries[idx];
                if !all_closed && !is_open(entry) {
                    if self.queued {
                        pool_of(entry).forget(self.id);
                    }
                    continue;
                }
                let pool = pool_of(entry);
                if let Poll::Ready(slot) = pool.poll_take(self.id, cx) {
                    if !all_closed && !is_open(entry) {
                        pool.give(slot);
                        continue;
                    }
                    self.forget_many(entries, &pool_of);
                    return Poll::Ready(Some((idx, slot)));
                }
                self.queued = true;
            }
            if entries.iter().filter(|e| !is_open(e)).count() == closed {
                if !all_closed {
                    return Poll::Pending;
                }
                self.forget_many(entries, &pool_of);
                return Poll::Ready(None);
            }
        }
    }

    /// Unregister from `pool` if queued.
    pub fn forget_one(&mut self, pool: &Pool) {
        if self.queued {
            self.queued = false;
            pool.forget(self.id);
        }
    }

    /// Unregister from all `entries` if queued.
    pub fn forget_many<T>(&mut self, entries: &[T], pool_of: impl Fn(&T) -> &Pool) {
        if self.queued {
            self.queued = false;
            for entry in entries {
                pool_of(entry).forget(self.id);
            }
        }
    }
}
