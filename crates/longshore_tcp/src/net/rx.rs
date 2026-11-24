// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving: what the transport delivers, kept until the caller pulls it.
//!
//! Ring mode: each ring buffer the armed receive delivers is queued on its
//! connection as a chunk. Direct mode: a frame's header is received into
//! the connection's header area, then the whole frame into frame memory.
//! Cutting chunks into frames happens when the caller pulls
//! (`pull.rs`).

use super::state::{best_effort, Chunk, St, EV_READABLE, NONE, W_READ};
use super::Net;
use crate::config::Rx;
use crate::event::Why;
use crate::framing::Framing;
use crate::transport::{Span, Transport};
use longshore_common::io::{BufId, ECANCELED, ENOBUFS};

impl<T: Transport> Net<T> {
    /// Where connection `slot` keeps a header that arrives in pieces.
    pub(super) fn header_area(&self, slot: u32) -> (BufId, u32) {
        let per = self.cfg.headers_per_buf;
        let buf = BufId::new(self.cfg.header_first.get() + slot / per);
        (buf, (slot % per) * self.cfg.sizes.header_max)
    }

    /// Start or continue reading on `slot`, if it should be reading.
    pub(super) fn start_reading(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.st != St::Up || c.paused || c.cut_off {
            return;
        }
        match c.opts.rx {
            Rx::Ring => {
                if c.armed {
                    return;
                }
                if self.t.recv_armed(d, slot).is_ok() {
                    self.conns[slot as usize].armed = true;
                } else {
                    self.add_work(slot, W_READ);
                }
            }
            Rx::Direct => self.read_direct(d, slot),
        }
    }

    /// Post the next receive of a direct connection: the rest of a header,
    /// or the rest of a frame.
    fn read_direct(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.recv_busy || c.ready.is_some() {
            return;
        }
        if c.held >= c.opts.held_max {
            self.conns[slot as usize].paused = true;
            return;
        }
        let into = match (c.opts.framing, c.asm) {
            (_, Some(ext)) => Span::new(ext.buf(), c.asm_have, c.asm_len - c.asm_have),
            (Framing::Header(h), None) if c.hdr_have < h.header_len() as u32 => {
                let (buf, off) = self.header_area(slot);
                Span::new(buf, off + c.hdr_have, h.header_len() as u32 - c.hdr_have)
            }
            (Framing::Header(_), None) => {
                // The header is in; its frame is waiting for frame memory.
                if self.begin_frame(d, slot) {
                    self.read_direct(d, slot);
                } else if self.conns[slot as usize].st == St::Up {
                    self.add_work(slot, W_READ);
                }
                return;
            }
            (Framing::Bytes, None) => {
                let Some(ext) = self.extents.take() else {
                    return self.add_work(slot, W_READ);
                };
                let size = self.cfg.sizes.frame_size;
                let reading = &mut self.conns[slot as usize];
                (reading.asm, reading.asm_have, reading.asm_len) = (Some(ext), 0, size);
                Span::new(ext.buf(), 0, size)
            }
        };
        match self.t.recv_into(d, slot, into) {
            Ok(None) => self.conns[slot as usize].recv_busy = true,
            // Plaintext that was there already: no completion follows.
            Ok(Some(result)) => self.on_received(d, slot, result),
            Err(_) => self.add_work(slot, W_READ),
        }
    }

    /// The header in `slot`'s header area is whole: take frame memory for
    /// its frame and move the header there. `false` when there is no frame
    /// memory right now, or the header announces no acceptable frame (the
    /// connection then ends).
    pub(super) fn begin_frame(&mut self, d: &mut T::Driver, slot: u32) -> bool {
        let Framing::Header(h) = self.conns[slot as usize].opts.framing else {
            return false;
        };
        let (hbuf, hoff) = self.header_area(slot);
        let hl = h.header_len();
        let header = &self.t.bytes(d, hbuf)[hoff as usize..hoff as usize + hl];
        let Ok(frame_len) = h.frame_len(header) else {
            self.broke_framing(slot);
            return false;
        };
        let Some(ext) = self.extents.take() else {
            return false;
        };
        let (src, dst) = self.t.bytes_pair(d, hbuf, ext.buf());
        dst[..hl].copy_from_slice(&src[hoff as usize..hoff as usize + hl]);
        let c = &mut self.conns[slot as usize];
        c.hdr_have = 0;
        if frame_len == hl {
            c.ready = Some((ext, frame_len as u32));
            self.queue_event(slot, EV_READABLE);
        } else {
            (c.asm, c.asm_have, c.asm_len) = (Some(ext), hl as u32, frame_len as u32);
        }
        true
    }

    /// `poll` found `slot` wanting to read again.
    pub(super) fn resume_reading(&mut self, d: &mut T::Driver, slot: u32) {
        if self.conns[slot as usize].unopened {
            // What the transport holds had no frame memory to go to.
            let _readable: bool = self.open_ring(d, slot);
        }
        if self.has_input(slot) {
            // Input the caller stopped pulling, or could not pull for
            // lack of frame memory: say so again.
            self.queue_event(slot, EV_READABLE);
        }
        if self.conns[slot as usize].starved {
            // The end that was held back for it, behind that input.
            self.owe_end(slot);
        }
        self.start_reading(d, slot);
    }

    pub(super) fn on_received(&mut self, d: &mut T::Driver, slot: u32, result: i32) {
        let c = &mut self.conns[slot as usize];
        c.recv_busy = false;
        if c.st != St::Up {
            return self.try_free(slot);
        }
        let Ok(n) = u32::try_from(result) else {
            return self.end(slot, Why::of(result));
        };
        if n == 0 {
            return self.peer_finished(slot);
        }
        c.last_active = self.now;
        match (c.opts.framing, c.asm) {
            (Framing::Bytes, Some(ext)) => {
                (c.asm, c.ready) = (None, Some((ext, n)));
                self.queue_event(slot, EV_READABLE);
            }
            (_, Some(ext)) => {
                c.asm_have += n;
                if c.asm_have == c.asm_len {
                    (c.asm, c.ready) = (None, Some((ext, c.asm_len)));
                    self.queue_event(slot, EV_READABLE);
                } else {
                    self.read_direct(d, slot);
                }
            }
            (_, None) => {
                c.hdr_have += n;
                self.read_direct(d, slot);
            }
        }
    }

    pub(super) fn on_ring_data(
        &mut self,
        d: &mut T::Driver,
        slot: u32,
        buf: BufId,
        len: u32,
        armed: bool,
    ) {
        let c = &mut self.conns[slot as usize];
        c.armed = armed;
        if c.st != St::Up {
            self.returns[self.returns_len] = buf;
            self.returns_len += 1;
            return self.try_free(slot);
        }
        c.last_active = self.now;
        let i = buf.get() - self.cfg.ring_first.get();
        self.chunks[i as usize] = Chunk {
            off: 0,
            len,
            next: NONE,
        };
        match c.chunk_tail {
            NONE => c.chunk_head = i,
            tail => self.chunks[tail as usize].next = i,
        }
        c.chunk_tail = i;
        if !c.ring_listed {
            c.ring_listed = true;
            self.ring_pending[self.ring_pending_len] = slot;
            self.ring_pending_len += 1;
        }
        // Protected: readable once a record has been opened.
        if !c.secure || self.open_ring(d, slot) {
            self.queue_event(slot, EV_READABLE);
        }
        if !armed {
            self.start_reading(d, slot);
        }
    }

    pub(super) fn on_recv_ended(&mut self, slot: u32, result: i32) {
        let c = &mut self.conns[slot as usize];
        c.armed = false;
        if c.st != St::Up {
            return self.try_free(slot);
        }
        match result {
            0 if c.secure => {
                // No close alert so far: broken, unless one is still to be
                // opened among what was received.
                c.cut_off = true;
                self.end_if_opened(slot);
            }
            0 => self.peer_finished(slot),
            // The ring was empty, or we stopped the receive ourselves:
            // armed again on the next `poll` unless reading is paused.
            r if r == -ENOBUFS || r == -ECANCELED => self.add_work(slot, W_READ),
            _ => self.end(slot, Why::of(result)),
        }
    }

    /// The peer ended the stream: in order, unless it turns out to have
    /// ended inside a frame once the caller has pulled what was received.
    pub(super) fn peer_finished(&mut self, slot: u32) {
        self.end(slot, Why::Orderly);
    }

    /// Stop reading on `slot` until the caller releases frame memory.
    pub(super) fn pause(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &mut self.conns[slot as usize];
        if c.paused {
            return;
        }
        c.paused = true;
        if c.armed {
            // If the driver has no room, the receive simply runs on until
            // the ring is empty; nothing is lost.
            best_effort(self.t.cancel_recv(d, slot));
        }
    }

    /// Give `slot`'s ring buffers back on the next `poll`.
    pub(super) fn drop_chunks(&mut self, slot: u32) {
        let mut i = core::mem::replace(&mut self.conns[slot as usize].chunk_head, NONE);
        self.conns[slot as usize].chunk_tail = NONE;
        while i != NONE {
            self.returns[self.returns_len] = BufId::new(self.cfg.ring_first.get() + i);
            self.returns_len += 1;
            i = self.chunks[i as usize].next;
        }
    }

    /// Release the frame memory `slot` holds for frames not yet delivered.
    pub(super) fn drop_rx(&mut self, slot: u32) {
        let c = &mut self.conns[slot as usize];
        let held = [
            c.asm.take(),
            c.ready.take().map(|(e, _)| e),
            c.backlog.take(),
        ];
        (c.hdr_have, c.asm_have, c.asm_len) = (0, 0, 0);
        (c.backlog_off, c.backlog_len, c.paused) = (0, 0, false);
        (c.unopened, c.cut_off) = (false, false);
        for ext in held.into_iter().flatten() {
            // Net's own hold; frames the caller pulled keep theirs.
            best_effort(self.extents.release(ext));
        }
    }
}
