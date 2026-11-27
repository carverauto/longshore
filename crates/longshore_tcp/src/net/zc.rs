// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving in pages (`Rx::ZeroCopy`): direct mode, with a frame whose
//! blocks arrive lying in whole pages of the backend's receive area lent
//! as those pages.
//!
//! A connection reads in two receives a frame, each armed with a credit
//! the kernel holds it to: the rest of the header, then exactly the rest
//! of the frame. Every byte a receive can bring has a place before it is
//! asked for: the header area, or the frame memory the connection took
//! before its body's receive (its landing). So a piece that is not kept
//! is copied and returned at once, and nothing received waits for
//! memory.
//!
//! A frame is kept in its pages when its header came as one piece at the
//! start of a page and every piece after it goes on where the last
//! stopped, block for block; its last, partial block is copied to a tail
//! block of its own. The first piece that does not go on in step makes
//! it a moved frame on the spot: what was kept is copied into the
//! landing, and the rest follows there.

use super::cover::Col;
use super::state::{best_effort, St, EV_READABLE, NEVER, NONE, W_READ, W_WAKE};
use super::Net;
use crate::config::Rx;
use crate::event::Why;
use crate::framing::Framing;
use crate::transport::{Refused, Transport};
use crate::Addr;
use longshore_common::io::extents::Extent;
use longshore_common::io::{BufId, Limits, ECANCELED, ENOMEM, ENOSPC};

/// A piece as the transport reported it.
#[derive(Clone, Copy)]
pub(super) struct Piece {
    pub(super) page: BufId,
    pub(super) off: u32,
    pub(super) len: u32,
}

impl<T: Transport> Net<T> {
    /// Whether the set can read in pages with `opts`: it has an area and
    /// pages for it, frames with a header, and none at a block. Always
    /// for the other modes.
    pub(super) fn fits_zc(&self, opts: &crate::config::ConnOpts) -> bool {
        if opts.rx != Rx::ZeroCopy {
            return true;
        }
        let sized = self.cfg.area_count > 0 && self.cfg.sizes.zc_pages > 0;
        match opts.framing {
            Framing::Header(h) => sized && h.body_block().is_none(),
            Framing::Bytes => false,
        }
    }

    /// Whether the mode refuses `addr`, or a port shared between rings:
    /// the packets of a Unix-domain socket reach no queue, and a shared
    /// port lets the kernel hand a connection to a ring whose queue its
    /// packets do not reach.
    pub(super) fn zc_refuses(opts: &crate::config::ConnOpts, addr: &Addr, shared: bool) -> bool {
        opts.rx == Rx::ZeroCopy && (matches!(addr, Addr::Unix(_)) || shared)
    }

    /// The header length of `slot`'s framing; 0 without a header.
    fn header_len(&self, slot: u32) -> u32 {
        match self.conns[slot as usize].opts.framing {
            Framing::Header(h) => h.header_len() as u32,
            Framing::Bytes => 0,
        }
    }

    /// Arm the next receive of a connection of the mode: for the rest of
    /// its header, or, with frame memory taken, the rest of its frame.
    pub(super) fn read_zc(&mut self, d: &mut T::Driver, slot: u32) {
        let hl = self.header_len(slot);
        let c = &self.conns[slot as usize];
        if c.armed || c.ready.is_some() || c.ready_run != NONE {
            return;
        }
        if c.held >= c.opts.held_max {
            self.conns[slot as usize].paused = true;
            return;
        }
        let credit = match c.asm {
            Some(_) => c.asm_len - c.asm_have,
            None if c.hdr_have < hl => hl - c.hdr_have,
            None => {
                // The header is in: its frame takes its landing first.
                if self.begin_zc(d, slot) {
                    return self.read_zc(d, slot);
                }
                if self.conns[slot as usize].st == St::Up {
                    self.add_work(slot, W_READ);
                }
                return;
            }
        };
        if credit != 0 && self.t.recv_zc(d, slot, credit).is_ok() {
            let reading = &mut self.conns[slot as usize];
            (reading.armed, reading.cancelling, reading.zc_want) = (true, false, credit);
        } else {
            self.add_work(slot, W_READ);
        }
    }

    /// The header of `slot` is whole: take the frame's landing, and decide
    /// whether the frame is kept in its pages. `false` without frame
    /// memory, or when the header announces no acceptable frame.
    fn begin_zc(&mut self, d: &mut T::Driver, slot: u32) -> bool {
        if !self.begin_frame(d, slot) {
            return false;
        }
        let hl = self.header_len(slot);
        let c = &mut self.conns[slot as usize];
        let header = core::mem::replace(&mut c.hdr_page, NONE);
        let len = c.asm_len;
        // A frame of its header alone is ready in its landing already.
        let body = c.asm.is_some();
        if header != NONE {
            self.zc.headers -= 1;
        }
        // At least a whole page, so that the first page, the header's, is
        // one the frame fills.
        let least = self.cfg.sizes.zc_min_frame.max(Limits::PAGE);
        let fits = body && header != NONE && len >= least;
        let page = BufId::new(header);
        let run = if fits { self.zc.take(len) } else { None };
        let kept = run.filter(|index| {
            let listed = self.zc.push(*index, page, 0, hl);
            if !listed {
                self.zc.retire(*index);
            }
            listed
        });
        match kept {
            Some(index) => self.conns[slot as usize].zc_run = index,
            None => {
                if header != NONE {
                    self.t.give_back_piece(d, page, 0, hl);
                }
                self.zc.frames_moved += 1;
            }
        }
        true
    }

    /// A zero-copy receive of `slot` delivered a piece.
    pub(super) fn on_piece(&mut self, d: &mut T::Driver, slot: u32, piece: Piece) {
        self.cover(slot, Col::Data);
        let c = &mut self.conns[slot as usize];
        c.zc_want = c.zc_want.saturating_sub(piece.len);
        c.no_page_since = NEVER;
        if c.st != St::Up {
            self.t.give_back_piece(d, piece.page, piece.off, piece.len);
            return;
        }
        c.last_active = self.now;
        match c.asm {
            Some(ext) => self.body_piece(d, slot, ext, piece),
            None => self.header_piece(d, slot, piece),
        }
    }

    /// A piece of a header: copied to the header area. A header that is
    /// one piece from the start of its page is kept out for the decision;
    /// any other goes back at once.
    fn header_piece(&mut self, d: &mut T::Driver, slot: u32, piece: Piece) {
        let hl = self.header_len(slot);
        let have = self.conns[slot as usize].hdr_have;
        let take = piece.len.min(hl.saturating_sub(have));
        let (hbuf, hoff) = self.header_area(slot);
        let (from, to) = self.t.bytes_pair(d, piece.page, hbuf);
        let (at, into) = (piece.off as usize, (hoff + have) as usize);
        to[into..into + take as usize].copy_from_slice(&from[at..at + take as usize]);
        self.zc.moved_bytes += u64::from(take);
        let c = &mut self.conns[slot as usize];
        c.hdr_have += take;
        if have == 0 && piece.off == 0 && piece.len == hl {
            c.hdr_page = piece.page.get();
            self.zc.headers += 1;
        } else {
            self.t.give_back_piece(d, piece.page, piece.off, piece.len);
        }
    }

    /// A piece of a frame's body, with the frame's landing at hand.
    fn body_piece(&mut self, d: &mut T::Driver, slot: u32, ext: Extent, piece: Piece) {
        let c = &self.conns[slot as usize];
        let (run, at) = (c.zc_run, c.asm_have);
        let whole = c.asm_len / Limits::PAGE * Limits::PAGE;
        if run != NONE && at < whole {
            let goes_on = self.zc.last(run).is_some_and(|last| {
                piece.off == 0 || (last.page == piece.page && last.off + last.len == piece.off)
            });
            let in_step = piece.off == at % Limits::PAGE && goes_on;
            if in_step && self.zc.push(run, piece.page, piece.off, piece.len) {
                return self.took(slot, piece.len);
            }
            self.break_frame(d, slot, ext);
        }
        // The last, partial block of a kept frame goes to its tail block;
        // every piece of a moved frame to the landing.
        let kept = self.conns[slot as usize].zc_run;
        let tail = if kept == NONE {
            None
        } else {
            self.zc.tail(kept)
        };
        let (buf, into) = match tail {
            Some(tail) => (tail.buf(), tail.off() + (at - whole)),
            None => (ext.buf(), at),
        };
        let (from, to) = self.t.bytes_pair(d, piece.page, buf);
        let (src, dst, n) = (piece.off as usize, into as usize, piece.len as usize);
        to[dst..dst + n].copy_from_slice(&from[src..src + n]);
        self.zc.moved_bytes += u64::from(piece.len);
        self.t.give_back_piece(d, piece.page, piece.off, piece.len);
        self.took(slot, piece.len);
        if let Some(tail) = tail.filter(|_| self.conns[slot as usize].ready_run == kept) {
            // Whole: what the tail block holds beyond the frame is zero.
            let used = (tail.off() + self.zc.len(kept) % Limits::PAGE) as usize;
            let end = (tail.off() + Limits::PAGE) as usize;
            self.t.bytes(d, tail.buf())[used..end].fill(0);
        }
    }

    /// `len` more bytes of `slot`'s frame are in: with the last of them
    /// the frame is ready, as its run or in its landing.
    fn took(&mut self, slot: u32, len: u32) {
        let c = &mut self.conns[slot as usize];
        c.asm_have += len;
        if c.asm_have < c.asm_len {
            return;
        }
        let whole = c.asm_len;
        let run = core::mem::replace(&mut c.zc_run, NONE);
        (c.asm_have, c.asm_len) = (0, 0);
        if run == NONE {
            c.ready = c.asm.take().map(|ext| (ext, whole));
        } else {
            // Kept: the landing was not needed.
            c.ready_run = run;
            if let Some(ext) = c.asm.take() {
                best_effort(self.extents.release(ext));
            }
        }
        self.queue_event(slot, EV_READABLE);
    }

    /// A piece of `slot`'s kept frame did not go on in step, or no record
    /// was left for it: the frame is a moved one from here. What was kept
    /// is copied into the landing, behind the header that is there
    /// already, and goes back.
    fn break_frame(&mut self, d: &mut T::Driver, slot: u32, ext: Extent) {
        let run = core::mem::replace(&mut self.conns[slot as usize].zc_run, NONE);
        let hl = self.header_len(slot);
        let (mut at, mut into) = (self.zc.first(run), 0u32);
        while at != NONE {
            let (rec, next) = self.zc.rec(at);
            if into >= hl {
                let (from, to) = self.t.bytes_pair(d, rec.page, ext.buf());
                let (src, dst, n) = (rec.off as usize, into as usize, rec.len as usize);
                to[dst..dst + n].copy_from_slice(&from[src..src + n]);
                self.zc.moved_bytes += u64::from(rec.len);
            }
            (into, at) = (into + rec.len, next);
        }
        self.zc.retire(run);
        self.zc.frames_moved += 1;
    }

    /// A zero-copy receive of `slot` ended.
    pub(super) fn on_zc_ended(&mut self, d: &mut T::Driver, slot: u32, result: i32) {
        self.cover(slot, Col::of_receive(result.min(0)));
        let c = &mut self.conns[slot as usize];
        (c.armed, c.cancelling) = (false, false);
        if c.st != St::Up {
            return self.try_free(slot);
        }
        let again = result == -ECANCELED || result == -ENOSPC || result == -ENOMEM;
        if c.zc_want == 0 && (result == 0 || again) {
            // Its credit is used: on to the frame's next receive. A cancel
            // may come after the last piece and before the end.
            return self.start_reading(d, slot);
        }
        match result {
            0 => self.peer_finished(slot),
            r if r == -ENOMEM => {
                // The kernel's copy found no page. Asked for again, for a
                // bounded time since the first such end without a byte.
                self.zc.area_short += 1;
                if c.no_page_since == NEVER {
                    c.no_page_since = self.now;
                }
                if self.now.saturating_sub(c.no_page_since) >= c.opts.no_page_timeout_ns {
                    return self.end(slot, Why::of(result));
                }
                self.add_work(slot, W_READ);
            }
            r if r == -ENOSPC || r == -ECANCELED => {
                // No room to report in, or stopped by the set: the rest
                // of the credit is asked for again.
                self.zc.rearmed += u64::from(r == -ENOSPC);
                self.add_work(slot, W_READ);
            }
            _ => self.end(slot, Why::of(result)),
        }
    }

    /// A turn in which the driver found no room to report in: a receive
    /// of the mode with credit left may stay silent though bytes wait
    /// for it. Each is stopped, once, and armed again when its end comes.
    pub(super) fn wake_silent(&mut self, d: &mut T::Driver) {
        if self.cfg.sizes.zc_pages == 0 || !self.t.take_ring_full(d) {
            return;
        }
        for slot in 0..self.cfg.conns() {
            self.wake(d, slot);
        }
    }

    /// Stop `slot`'s receive of the mode, if one is armed and not being
    /// stopped already. A driver with no room for the cancel is asked
    /// again on the next `poll`: the flag that said the receive may be
    /// silent is spent.
    pub(super) fn wake(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.opts.rx != Rx::ZeroCopy || c.st != St::Up || !c.armed || c.cancelling {
            return;
        }
        match self.t.cancel_recv(d, slot) {
            Ok(()) => self.conns[slot as usize].cancelling = true,
            Err(Refused::Busy) => self.add_work(slot, W_WAKE),
            Err(Refused::Unsupported) => {}
        }
    }

    /// Give up what `slot` holds of the mode for a frame not delivered:
    /// its header's piece, the run of the frame that is arriving, a run
    /// that is ready. They go back on the next `poll`.
    pub(super) fn drop_zc(&mut self, slot: u32) {
        let hl = self.header_len(slot);
        let c = &mut self.conns[slot as usize];
        let header = core::mem::replace(&mut c.hdr_page, NONE);
        let runs = [
            core::mem::replace(&mut c.zc_run, NONE),
            core::mem::replace(&mut c.ready_run, NONE),
        ];
        (c.zc_want, c.no_page_since) = (0, NEVER);
        if header != NONE {
            self.zc.headers -= 1;
            self.zc.queue(BufId::new(header), 0, hl);
        }
        for run in runs {
            if run != NONE {
                self.zc.retire(run);
            }
        }
    }

    /// Give the pieces that wait back to the transport, and free the runs
    /// nothing holds any more.
    pub(super) fn return_pieces(&mut self, d: &mut T::Driver) {
        let Net { zc, t, .. } = self;
        zc.drain(|page, off, len| t.give_back_piece(d, page, off, len));
    }

    /// Return every piece the set holds, whatever holds it, and free its
    /// frames in pages: before a `Net` of the mode is dropped while its
    /// backend lives on. Frames of the set are not read afterwards. A
    /// `Net` dropped without it leaves its pieces out until the backend
    /// is dropped, and a second set on that backend a smaller area.
    pub fn give_back_all(&mut self, d: &mut T::Driver) {
        for slot in 0..self.cfg.conns() {
            self.drop_zc(slot);
        }
        self.zc.retire_all();
        self.return_pieces(d);
    }
}
