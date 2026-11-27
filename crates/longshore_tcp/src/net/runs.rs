// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Frames in pages: the runs that list their pieces, the tail blocks,
//! the page budget, and the pieces waiting to go back.
//!
//! A frame lent in the pages it arrived in is named by a run: the pieces
//! of its whole pages in order, and a block of its own for its last,
//! partial block, padded with zeros. A run has the caller's hold, which
//! `Net::release` and `Net::hold` drop, and a count of other holds, one
//! for each file write that reads the pages. Its pieces go back to the
//! backend when both are gone, and not before. Every table here is sized
//! when the `Net` is built.

use super::state::NONE;
use crate::config::Config;
use crate::event::NetError;
use crate::transport::Span;
use longshore_common::io::{BufId, Limits};

/// A frame in pages, for a hold by something outside the `Net`: what
/// [`super::Frame::run`] gives, and [`Pages::hold`] and
/// [`Pages::release`] take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    index: u32,
    generation: u32,
}

/// A file write's hold on a run, from [`Pages::hold`], given back with
/// [`Pages::release`], which takes it. It cannot be copied or made outside
/// the `Net`: no write can give back another's, nor its own twice.
#[must_use = "a hold never given back keeps the run's pages from the card"]
#[derive(PartialEq, Eq, Debug)]
pub struct WriteHold {
    run: Run,
    once: Once,
}

/// What makes a [`WriteHold`] move, not copy, wherever it goes.
#[derive(PartialEq, Eq, Debug)]
struct Once;

impl WriteHold {
    /// The run held.
    pub const fn run(&self) -> Run {
        self.run
    }
}

/// One piece of a run: `len` bytes of `page` from `off`.
#[derive(Clone, Copy)]
pub(super) struct Rec {
    pub(super) page: BufId,
    pub(super) off: u32,
    pub(super) len: u32,
    /// The run's next piece, or the next free record; or `NONE`.
    next: u32,
}

#[derive(Clone, Copy)]
struct RunState {
    generation: u32,
    live: bool,
    /// The hold of the caller the frame was delivered to.
    caller: bool,
    /// Holds of file writes.
    holds: u32,
    head: u32,
    last: u32,
    /// Pages of the budget it took.
    pages: u32,
    /// Bytes of the frame.
    len: u32,
    /// Its tail block, or `NONE` for a frame of whole blocks.
    tail: u32,
}

/// The runs of a `Net`, and what goes with them.
pub struct Pages {
    runs: Box<[RunState]>,
    run_free: Box<[u32]>,
    run_free_len: usize,
    recs: Box<[Rec]>,
    rec_free: u32,
    recs_out: u32,
    tail_free: Box<[u32]>,
    tail_free_len: usize,
    tail_first: BufId,
    tails_per_buf: u32,
    /// Pages of the budget not taken, and the budget.
    budget: u32,
    pages_max: u32,
    /// Runs nothing holds any more: their pieces go back, and they are
    /// free, on the next `poll`, when the driver is at hand.
    retired: Box<[u32]>,
    retired_len: usize,
    /// Headers' pieces to give back on the next `poll`.
    returns: Box<[(BufId, u32, u32)]>,
    returns_len: usize,
    /// Headers of one piece kept while their connections wait for frame
    /// memory.
    pub(super) headers: u32,
    pub(super) area_short: u64,
    pub(super) rearmed: u64,
    pub(super) frames_kept: u64,
    pub(super) frames_moved: u64,
    pub(super) moved_bytes: u64,
}

impl Pages {
    pub(super) fn new(cfg: &Config) -> Pages {
        let s = cfg.sizes;
        let quiet = RunState {
            generation: 0,
            live: false,
            caller: false,
            holds: 0,
            head: NONE,
            last: NONE,
            pages: 0,
            len: 0,
            tail: NONE,
        };
        // Without pages no connection reads in them: no tables either.
        let (frames, recs) = if s.zc_pages == 0 {
            (0, 0)
        } else {
            (s.zc_frames, s.zc_pieces)
        };
        Pages {
            runs: vec![quiet; frames as usize].into_boxed_slice(),
            run_free: (0..frames).rev().collect(),
            run_free_len: frames as usize,
            // Free records are chained through `next`.
            recs: (0..recs)
                .map(|i| Rec {
                    page: BufId::new(0),
                    off: 0,
                    len: 0,
                    next: if i + 1 < recs { i + 1 } else { NONE },
                })
                .collect(),
            rec_free: if recs == 0 { NONE } else { 0 },
            recs_out: 0,
            tail_free: (0..frames).rev().collect(),
            tail_free_len: frames as usize,
            tail_first: cfg.tail_first,
            tails_per_buf: (s.frame_size / Limits::PAGE).max(1),
            budget: s.zc_pages,
            pages_max: s.zc_pages,
            retired: vec![0; frames as usize].into_boxed_slice(),
            retired_len: 0,
            // A header a connection: a slot holds one at a time, and what
            // it gives up goes back with the completion it came in, or
            // with the next call that has the driver. Twice that, to spare.
            returns: vec![(BufId::new(0), 0, 0); 2 * cfg.conns() as usize].into_boxed_slice(),
            returns_len: 0,
            headers: 0,
            area_short: 0,
            rearmed: 0,
            frames_kept: 0,
            frames_moved: 0,
            moved_bytes: 0,
        }
    }

    /// A hold on `run` for something outside the `Net` that reads its
    /// pages: a file write, until it and its sync have completed.
    pub fn hold(&mut self, run: Run) -> Result<WriteHold, NetError> {
        let state = self.live(run).ok_or(NetError::Stale)?;
        state.holds = state.holds.checked_add(1).ok_or(NetError::Stale)?;
        Ok(WriteHold { run, once: Once })
    }

    /// Give back a hold [`Pages::hold`] gave. Whether nothing holds the
    /// run any longer: the caller's hold was gone already, and this was the
    /// last of the others; its pages then go back to the card at the next
    /// `poll`. Stale for a run the set gave back whole meanwhile
    /// (`Net::give_back_all`).
    pub fn release(&mut self, hold: WriteHold) -> Result<bool, NetError> {
        // Taken apart, the hold is gone: it cannot be given back again.
        let WriteHold { run, once: _spent } = hold;
        let state = self.live(run).ok_or(NetError::Stale)?;
        state.holds = state.holds.checked_sub(1).ok_or(NetError::Stale)?;
        Ok(self.settle(run.index))
    }

    /// Pages frames in pages hold, and the budget.
    pub const fn kept(&self) -> (u32, u32) {
        (self.pages_max - self.budget, self.pages_max)
    }

    fn live(&mut self, run: Run) -> Option<&mut RunState> {
        self.runs
            .get_mut(run.index as usize)
            .filter(|r| r.live && r.generation == run.generation)
    }

    /// The handle of run `index`.
    pub(super) fn run(&self, index: u32) -> Run {
        Run {
            index,
            generation: self.runs[index as usize].generation,
        }
    }

    /// The place of `run`, if it is still what the handle named.
    pub(super) fn index_of(&self, run: Run) -> Option<u32> {
        let state = self.runs.get(run.index as usize)?;
        (state.live && state.generation == run.generation).then_some(run.index)
    }

    /// Whether the caller's hold of run `index` is still there.
    pub(super) fn caller_holds(&self, index: u32) -> bool {
        self.runs[index as usize].caller
    }

    /// Runs in use, and piece records in use.
    pub(super) fn in_use(&self) -> (u32, u32) {
        let runs = self.runs.len() - self.run_free_len;
        (runs as u32, self.recs_out)
    }

    /// A run for a frame of `len` bytes: `None` when no run is free, no
    /// tail block for its last, partial block, or the budget has not its
    /// whole pages.
    pub(super) fn take(&mut self, len: u32) -> Option<u32> {
        let (pages, partial) = (len / Limits::PAGE, !len.is_multiple_of(Limits::PAGE));
        let run = self.run_free_len.checked_sub(1)?;
        if self.budget < pages || (partial && self.tail_free_len == 0) {
            return None;
        }
        let mut tail = NONE;
        if partial {
            self.tail_free_len -= 1;
            tail = self.tail_free[self.tail_free_len];
        }
        self.run_free_len = run;
        let index = self.run_free[run];
        self.budget -= pages;
        let state = &mut self.runs[index as usize];
        (state.live, state.caller, state.holds) = (true, false, 0);
        (state.head, state.last, state.pages) = (NONE, NONE, pages);
        (state.len, state.tail) = (len, tail);
        Some(index)
    }

    /// List a piece at the end of run `index`. `false` when no record is
    /// left.
    pub(super) fn push(&mut self, index: u32, page: BufId, off: u32, len: u32) -> bool {
        let at = self.rec_free;
        if at == NONE {
            return false;
        }
        self.rec_free = self.recs[at as usize].next;
        self.recs[at as usize] = Rec {
            page,
            off,
            len,
            next: NONE,
        };
        self.recs_out += 1;
        let state = &mut self.runs[index as usize];
        match state.last {
            NONE => state.head = at,
            last => self.recs[last as usize].next = at,
        }
        state.last = at;
        true
    }

    /// The last piece listed in run `index`.
    pub(super) fn last(&self, index: u32) -> Option<Rec> {
        let last = self.runs[index as usize].last;
        (last != NONE).then(|| self.recs[last as usize])
    }

    /// The first piece of run `index`, and from a piece the one after it.
    pub(super) fn first(&self, index: u32) -> u32 {
        self.runs[index as usize].head
    }

    /// Piece record `at` and the place of the one after it.
    pub(super) fn rec(&self, at: u32) -> (Rec, u32) {
        let rec = self.recs[at as usize];
        (rec, rec.next)
    }

    /// The tail block of run `index`: where its last, partial block lies.
    pub(super) fn tail(&self, index: u32) -> Option<Span> {
        let tail = self.runs[index as usize].tail;
        (tail != NONE).then(|| {
            let buf = BufId::new(self.tail_first.get() + tail / self.tails_per_buf);
            Span::new(
                buf,
                (tail % self.tails_per_buf) * Limits::PAGE,
                Limits::PAGE,
            )
        })
    }

    /// Bytes of the frame run `index` holds.
    pub(super) fn len(&self, index: u32) -> u32 {
        self.runs[index as usize].len
    }

    /// The frame of run `index` is delivered: the caller holds it. Its
    /// length.
    pub(super) fn deliver(&mut self, index: u32) -> u32 {
        self.runs[index as usize].caller = true;
        self.frames_kept += 1;
        self.runs[index as usize].len
    }

    /// Drop the caller's hold of run `index`. Whether its pages went back.
    pub(super) fn drop_caller(&mut self, index: u32) -> bool {
        self.runs[index as usize].caller = false;
        self.settle(index)
    }

    /// Retire run `index` when nothing holds it. Whether it was retired.
    fn settle(&mut self, index: u32) -> bool {
        let state = &self.runs[index as usize];
        if state.caller || state.holds > 0 {
            return false;
        }
        self.retire(index);
        true
    }

    /// Retire run `index` whatever holds it: handles to it are stale from
    /// now, and its pieces go back on the next `poll`.
    pub(super) fn retire(&mut self, index: u32) {
        let state = &mut self.runs[index as usize];
        if !state.live {
            return;
        }
        (state.live, state.caller, state.holds) = (false, false, 0);
        state.generation = state.generation.wrapping_add(1);
        self.retired[self.retired_len] = index;
        self.retired_len += 1;
    }

    /// Every run in use, whatever holds it: for giving a set back whole.
    pub(super) fn retire_all(&mut self) {
        for index in 0..self.runs.len() as u32 {
            self.retire(index);
        }
    }

    /// A header's piece to give back on the next `poll`.
    pub(super) fn queue(&mut self, page: BufId, off: u32, len: u32) {
        self.returns[self.returns_len] = (page, off, len);
        self.returns_len += 1;
    }

    /// Headers' pieces that wait to go back.
    pub(super) const fn queued(&self) -> u32 {
        self.returns_len as u32
    }

    /// Give the headers' pieces that wait to `back`.
    pub(super) fn drain_headers(&mut self, mut back: impl FnMut(BufId, u32, u32)) {
        for (page, off, len) in &self.returns[..core::mem::take(&mut self.returns_len)] {
            back(*page, *off, *len);
        }
    }

    /// Give every piece that waits to `back`, and free the retired runs:
    /// their records, their tail blocks, their pages of the budget.
    pub(super) fn drain(&mut self, mut back: impl FnMut(BufId, u32, u32)) {
        self.drain_headers(&mut back);
        for i in 0..core::mem::take(&mut self.retired_len) {
            let index = self.retired[i];
            let state = self.runs[index as usize];
            let mut at = state.head;
            while at != NONE {
                let rec = self.recs[at as usize];
                back(rec.page, rec.off, rec.len);
                self.recs[at as usize].next = self.rec_free;
                (self.rec_free, at) = (at, rec.next);
                self.recs_out -= 1;
            }
            if state.tail != NONE {
                self.tail_free[self.tail_free_len] = state.tail;
                self.tail_free_len += 1;
            }
            self.budget += state.pages;
            (
                self.runs[index as usize].head,
                self.runs[index as usize].last,
            ) = (NONE, NONE);
            self.run_free[self.run_free_len] = index;
            self.run_free_len += 1;
        }
    }
}
