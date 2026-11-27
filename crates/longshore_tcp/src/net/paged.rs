// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Frames in pages as the caller sees them: read as spans, held into
//! frame memory, released.

use super::frames::{Frame, Mem};
use super::runs::{Pages, Run};
use super::state::NONE;
use super::Net;
use crate::event::NetError;
use crate::transport::{Span, Transport};
use longshore_common::io::Limits;

/// The spans of a frame in pages, in order: its whole pages, then the
/// block that holds its last, partial block. Every span but the last is
/// a whole page of the receive area; the last block is zero beyond the
/// frame to the end of its block, so a write of whole blocks may take
/// it whole. From [`Net::pages`].
pub struct Spans<'a> {
    pages: &'a Pages,
    at: u32,
    tail: Option<Span>,
}

impl Iterator for Spans<'_> {
    type Item = Span;

    fn next(&mut self) -> Option<Span> {
        if self.at == NONE {
            return self.tail.take();
        }
        // Pieces that go on in their page are one span.
        let (first, mut next) = self.pages.rec(self.at);
        let mut len = first.len;
        while next != NONE {
            let (rec, after) = self.pages.rec(next);
            if rec.page != first.page || rec.off != first.off + len {
                break;
            }
            (len, next) = (len + rec.len, after);
        }
        self.at = next;
        Some(Span::new(first.page, first.off, len))
    }
}

impl<T: Transport> Net<T> {
    /// The place of the run of a frame in pages the caller still holds.
    pub(super) fn held_run(&self, run: Run) -> Option<u32> {
        self.zc
            .index_of(run)
            .filter(|index| self.zc.caller_holds(*index))
    }

    /// Where the bytes of a frame in pages lie, in order; `None` for any
    /// other frame, or once it is released. [`Net::bytes`] is `None` for
    /// such a frame: it is not one run of memory.
    pub fn pages(&self, frame: &Frame) -> Option<Spans<'_>> {
        let Mem::Pages(run) = frame.mem else {
            return None;
        };
        let index = self.held_run(run)?;
        Some(self.spans_of(index))
    }

    /// The spans of run `index`.
    fn spans_of(&self, index: u32) -> Spans<'_> {
        let tail = self.zc.tail(index).map(|block| {
            let len = self.zc.len(index) % Limits::PAGE;
            Span::new(block.buf(), block.off(), len)
        });
        Spans {
            pages: &self.zc,
            at: self.zc.first(index),
            tail,
        }
    }

    /// [`Net::hold`] of a frame in pages: copied once into frame memory,
    /// which the caller holds from now on in place of the run. The run's
    /// pieces go back once no file write holds them either.
    pub(super) fn hold_pages(
        &mut self,
        d: &mut T::Driver,
        frame: Frame,
        run: Run,
    ) -> Result<Frame, NetError> {
        let index = self.held_run(run).ok_or(NetError::Stale)?;
        let ext = self.extents.take().ok_or(NetError::NoMemory)?;
        let mut into = 0usize;
        for span in self.spans_of(index) {
            let (from, to) = self.t.bytes_pair(d, span.buf(), ext.buf());
            let (at, n) = (span.off() as usize, span.len() as usize);
            to[into..into + n].copy_from_slice(&from[at..at + n]);
            into += n;
        }
        let _went: bool = self.zc.drop_caller(index);
        // Counted against `held_max` once: the count moves with the frame.
        Ok(Frame {
            conn: frame.conn,
            mem: Mem::Extent(ext),
            off: 0,
            len: frame.len,
        })
    }

    /// [`Net::release`] of a frame in pages: the caller's hold of its run
    /// is gone. A second release of the same frame, or one after
    /// [`Net::hold`], does nothing.
    pub(super) fn release_pages(&mut self, frame: &Frame, run: Run) {
        let Some(index) = self.held_run(run) else {
            return;
        };
        let _went: bool = self.zc.drop_caller(index);
        self.uncount_held(frame);
    }
}
