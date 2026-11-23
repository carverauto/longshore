// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Generic per-slot answer cells supporting async polling, blocking wait,
//! caller detach (freeing the slot before returning the caller-attached tag),
//! and closing unanswered sent slots without overwriting existing answers.

use crate::pool::Pool;
use crate::sync::{unpoisoned, Condvar, Mutex};
use core::task::{Context, Poll, Waker};

struct CellState<A> {
    sent: bool,
    answer: Option<A>,
    waker: Option<Waker>,
    tag: Option<u32>,
    detached: bool,
}

impl<A> Default for CellState<A> {
    fn default() -> Self {
        Self {
            sent: false,
            answer: None,
            waker: None,
            tag: None,
            detached: false,
        }
    }
}

struct Cell<A> {
    state: Mutex<CellState<A>>,
    done: Condvar,
}

impl<A> Cell<A> {
    fn new() -> Self {
        let cell = Self {
            state: Mutex::new(CellState::default()),
            done: Condvar::new(),
        };
        #[cfg(not(longshore_model))]
        {
            drop(cell.state.lock());
            cell.done.notify_one();
        }
        cell
    }
}

/// Fixed slice of answer cells indexed by slot number `0..slots`.
pub struct Cells<A> {
    cells: Box<[Cell<A>]>,
}

impl<A> Cells<A> {
    /// Allocate `slots` idle answer cells.
    pub fn new(slots: u32) -> Self {
        assert!(slots >= 1, "cells require at least one slot");
        Self {
            cells: (0..slots).map(|_| Cell::new()).collect(),
        }
    }

    /// Number of cells.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// True if empty (never true when constructed via [`Cells::new`]).
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Mark `slot` as sent with optional caller-attached `tag` before enqueueing
    /// the request.
    pub fn prepare(&self, slot: u32, tag: Option<u32>) {
        let mut s = unpoisoned(self.cells[slot as usize].state.lock());
        *s = CellState {
            sent: true,
            answer: None,
            waker: None,
            tag,
            detached: false,
        };
    }

    /// Roll back [`Cells::prepare`] after a failed send.
    pub fn cancel_prepare(&self, slot: u32) {
        let mut s = unpoisoned(self.cells[slot as usize].state.lock());
        *s = CellState::default();
    }

    /// Poll `slot` for an answer, freeing `slot` into `pool` once taken.
    pub fn poll_answer(&self, pool: &Pool, slot: u32, cx: &mut Context<'_>) -> Poll<A> {
        let mut s = unpoisoned(self.cells[slot as usize].state.lock());
        if let Some(ans) = s.answer.take() {
            *s = CellState::default();
            drop(s);
            pool.give(slot);
            return Poll::Ready(ans);
        }
        match &mut s.waker {
            Some(w) => w.clone_from(cx.waker()),
            None => s.waker = Some(cx.waker().clone()),
        }
        Poll::Pending
    }

    /// Block the calling thread until `slot` is answered, then free `slot`
    /// into `pool` and return the answer.
    pub fn wait_answer(&self, pool: &Pool, slot: u32) -> A {
        let cell = &self.cells[slot as usize];
        let mut s = unpoisoned(cell.state.lock());
        let ans = loop {
            if let Some(ans) = s.answer.take() {
                break ans;
            }
            s = unpoisoned(cell.done.wait(s));
        };
        *s = CellState::default();
        drop(s);
        pool.give(slot);
        ans
    }

    /// Caller gave up waiting on `slot`. If already answered, resets the cell,
    /// frees `slot` into `pool`, and returns `Some(answer)`. Otherwise marks the
    /// cell detached so the receiver will free `slot` and reclaim any `tag`.
    pub fn detach(&self, pool: &Pool, slot: u32) -> Option<A> {
        let mut s = unpoisoned(self.cells[slot as usize].state.lock());
        if let Some(ans) = s.answer.take() {
            *s = CellState::default();
            drop(s);
            pool.give(slot);
            return Some(ans);
        }
        s.detached = true;
        s.waker = None;
        None
    }

    /// Receiver records `value` for `slot`.
    ///
    /// If the caller detached before the answer arrived, clears the cell,
    /// frees `slot` into `pool` FIRST, and then returns the caller's attached
    /// `u32` tag (if any).
    pub fn answer(&self, pool: &Pool, slot: u32, value: A) -> Option<u32> {
        let cell = &self.cells[slot as usize];
        let mut s = unpoisoned(cell.state.lock());
        if s.detached {
            let tag = s.tag.take();
            *s = CellState::default();
            drop(s);
            pool.give(slot);
            return tag;
        }
        s.sent = false;
        s.answer = Some(value);
        let waker = s.waker.take();
        drop(s);
        cell.done.notify_one();
        if let Some(w) = waker {
            w.wake();
        }
        None
    }

    /// Answer every slot that is currently marked `sent` and unanswered using
    /// `closed()`, without overwriting existing answers or touching idle
    /// cells. Detached slots free their slot into `pool` before passing their
    /// attached `u32` tag to `on_detached`.
    pub fn close_unanswered(
        &self,
        pool: &Pool,
        mut closed: impl FnMut() -> A,
        mut on_detached: impl FnMut(u32),
    ) {
        for (slot, cell) in self.cells.iter().enumerate() {
            let mut s = unpoisoned(cell.state.lock());
            if !s.sent || s.answer.is_some() {
                continue;
            }
            if s.detached {
                let tag = s.tag.take();
                *s = CellState::default();
                drop(s);
                pool.give(slot as u32);
                if let Some(tag) = tag {
                    on_detached(tag);
                }
                continue;
            }
            s.sent = false;
            s.answer = Some(closed());
            let waker = s.waker.take();
            drop(s);
            cell.done.notify_one();
            if let Some(w) = waker {
                w.wake();
            }
        }
    }
}
