// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Sending: each connection has a queue of spans of registered memory.
//!
//! A frame the caller builds is written straight into a send segment;
//! frames queued back to back share a segment and go out in one send. A
//! frame the caller already holds in frame memory is queued where it lies
//! (`send_lent`), under one more hold. One send is in flight per
//! connection; its memory stays until the transport reports the send done.

use super::cover::Col;
use super::frames::Frame;
use super::state::{best_effort, Keep, St, TxSpan, W_FLUSH, W_REFUSE, W_SHUT};
use super::Net;
use crate::event::{NetError, SendError, Why};
use crate::handle::Conn;
use crate::transport::{Span, Transport};
use longshore_common::io::BufId;

impl<T: Transport> Net<T> {
    fn span_at(&self, slot: u32, i: u32) -> usize {
        let per = self.cfg.sizes.spans;
        (slot * per + (self.conns[slot as usize].tx_head + i) % per) as usize
    }

    fn segment_buf(&self, index: u32) -> BufId {
        BufId::new(self.cfg.segment_first.get() + index)
    }

    /// The slot of a connection that may be sent on.
    fn sendable(&self, conn: Conn) -> Result<u32, SendError> {
        let slot = self.gens.slot_of(conn).ok_or(SendError::Stale)?;
        let c = &self.conns[slot as usize];
        // A refused connection is sent one frame.
        let spent = c.shut || (c.st == St::Refused && c.answered);
        if !self.flushable(slot) || spent {
            return Err(SendError::NotUp);
        }
        Ok(slot)
    }

    /// Queue `len` bytes for `conn`, written by `fill` straight into send
    /// memory. They leave on the next `poll`, together with whatever else
    /// is queued behind them.
    pub fn send_frame(
        &mut self,
        d: &mut T::Driver,
        conn: Conn,
        len: u32,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<(), SendError> {
        self.cover_call(conn, Col::Send);
        let slot = self.sendable(conn)?;
        let size = self.cfg.sizes.segment_size;
        if len > size {
            return Err(SendError::TooLarge);
        }
        let c = &self.conns[slot as usize];
        // The last span can grow if it is a segment the transport is not
        // sending from right now.
        let last = c.tx_len.checked_sub(1).filter(|i| *i > 0 || !c.sending);
        let grow = last.map(|i| self.span_at(slot, i)).filter(|at| {
            let s = &self.spans[*at];
            matches!(s.keep, Keep::Segment(_)) && s.off + s.len + len <= size
        });
        let (buf, at) = if let Some(i) = grow {
            let s = &mut self.spans[i];
            s.len += len;
            (s.buf, s.off + s.len - len)
        } else {
            let buf = self.push_segment(slot, len)?;
            (buf, 0)
        };
        fill(&mut self.t.bytes(d, buf)[at as usize..(at + len) as usize]);
        self.queued(slot);
        Ok(())
    }

    /// Queue a copy of `bytes` for `conn`: `send_frame` for a caller whose
    /// bytes already exist somewhere else.
    pub fn send_bytes(
        &mut self,
        d: &mut T::Driver,
        conn: Conn,
        bytes: &[u8],
    ) -> Result<(), SendError> {
        let len = u32::try_from(bytes.len()).map_err(|_| SendError::TooLarge)?;
        self.send_frame(d, conn, len, |to| to.copy_from_slice(bytes))
    }

    /// A new span on a fresh segment, at the end of `slot`'s queue.
    fn push_segment(&mut self, slot: u32, len: u32) -> Result<BufId, SendError> {
        let c = &mut self.conns[slot as usize];
        if c.tx_len == self.cfg.sizes.spans {
            // Its own next send to finish makes room.
            c.full = true;
            return Err(SendError::Full);
        }
        let Some(top) = self.segs_len.checked_sub(1) else {
            // Any connection's next segment to return makes room.
            c.full = true;
            self.wait_for_segment(slot);
            return Err(SendError::Full);
        };
        c.full = false;
        self.segs_len = top;
        let index = self.segs[top];
        let buf = self.segment_buf(index);
        self.push_span(slot, Span::new(buf, 0, len), Keep::Segment(index));
        Ok(buf)
    }

    /// List `slot` among the connections waiting for a segment, once.
    fn wait_for_segment(&mut self, slot: u32) {
        let c = &mut self.conns[slot as usize];
        if !c.seg_listed {
            c.seg_listed = true;
            let at = (self.seg_wait_head + self.seg_wait_len) % self.seg_waiters.len();
            self.seg_waiters[at] = slot;
            self.seg_wait_len += 1;
        }
    }

    /// A segment is free again: tell the connection that has waited
    /// longest and still waits.
    fn segment_returned(&mut self) {
        while self.seg_wait_len > 0 {
            let slot = self.seg_waiters[self.seg_wait_head];
            self.seg_wait_head = (self.seg_wait_head + 1) % self.seg_waiters.len();
            self.seg_wait_len -= 1;
            let open = self.flushable(slot);
            let c = &mut self.conns[slot as usize];
            c.seg_listed = false;
            if c.full && open {
                c.full = false;
                return self.mark_writable(slot);
            }
        }
    }

    fn push_span(&mut self, slot: u32, span: Span, keep: Keep) {
        let at = self.span_at(slot, self.conns[slot as usize].tx_len);
        self.spans[at] = TxSpan {
            buf: span.buf(),
            off: span.off(),
            len: span.len(),
            keep,
        };
        let c = &mut self.conns[slot as usize];
        c.tx_len += 1;
        if c.tx_len == 1 {
            self.tx_busy += 1;
        }
    }

    /// Queue a frame the caller holds in frame memory for `conn`, from
    /// where it lies: nothing is copied. The `Net` takes a hold of its own
    /// until the send is done, so the caller may release the frame at once.
    pub fn send_lent(&mut self, conn: Conn, frame: &Frame) -> Result<(), SendError> {
        self.cover_call(conn, Col::Send);
        let slot = self.sendable(conn)?;
        let ext = frame.extent().ok_or(SendError::NotHeld)?;
        if self.conns[slot as usize].tx_len == self.cfg.sizes.spans {
            self.conns[slot as usize].full = true;
            return Err(SendError::Full);
        }
        self.extents.hold(ext).map_err(|_| SendError::Stale)?;
        self.push_span(slot, frame.span(), Keep::Extent(ext));
        self.queued(slot);
        Ok(())
    }

    /// Something was queued on `slot`: it goes out on the next `poll`.
    fn queued(&mut self, slot: u32) {
        let c = &mut self.conns[slot as usize];
        c.answered |= c.st == St::Refused;
        self.add_work(slot, W_FLUSH);
    }

    /// Whether a send on `conn` would be accepted, room in its queue
    /// aside: it is up, or half-closed, or refused and not yet sent its
    /// one frame, and its write side has not been shut down.
    pub fn can_send(&self, conn: Conn) -> bool {
        self.sendable(conn).is_ok()
    }

    /// Bytes queued and not yet sent on `conn`.
    pub fn send_queue(&self, conn: Conn) -> u32 {
        let Some(slot) = self.gens.slot_of(conn) else {
            return 0;
        };
        let n = self.conns[slot as usize].tx_len;
        (0..n).map(|i| self.spans[self.span_at(slot, i)].len).sum()
    }

    /// Send the front of `slot`'s queue, unless a send is in flight.
    pub(super) fn flush(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.sending || c.tx_len == 0 || !self.flushable(slot) {
            return;
        }
        let s = self.spans[self.span_at(slot, 0)];
        // The kernel copies for a Unix-domain socket whatever it is asked.
        // Nor is anything sealed for TLS sent from where the caller put it.
        let zero_copy = !c.unix && !c.secure && s.len >= self.cfg.sizes.zero_copy_min;
        if self
            .t
            .send(d, slot, Span::new(s.buf, s.off, s.len), zero_copy)
            .is_ok()
        {
            self.conns[slot as usize].sending = true;
        } else {
            self.add_work(slot, W_FLUSH);
        }
    }

    pub(super) fn on_sent(&mut self, d: &mut T::Driver, slot: u32, result: i32) {
        self.cover(slot, Col::Sent);
        self.conns[slot as usize].sending = false;
        if !self.flushable(slot) {
            return self.try_free(slot);
        }
        let c = &mut self.conns[slot as usize];
        let Ok(n) = u32::try_from(result) else {
            return self.end(slot, Why::of(result));
        };
        c.last_active = self.now;
        let at = self.span_at(slot, 0);
        let s = &mut self.spans[at];
        (s.off, s.len) = (s.off + n, s.len.saturating_sub(n));
        if s.len == 0 {
            self.pop_span(slot);
            if core::mem::take(&mut self.conns[slot as usize].full) {
                self.mark_writable(slot);
            }
        }
        let after = &self.conns[slot as usize];
        let (queued, shut, refused) = (after.tx_len > 0, after.shut, after.st == St::Refused);
        if queued {
            return self.flush(d, slot);
        }
        if shut {
            self.add_work(slot, W_SHUT);
        }
        if refused {
            self.add_work(slot, W_REFUSE);
        }
    }

    /// Remove the front span of `slot`'s queue and let go of its memory.
    fn pop_span(&mut self, slot: u32) {
        let s = self.spans[self.span_at(slot, 0)];
        match s.keep {
            Keep::Segment(index) => {
                self.segs[self.segs_len] = index;
                self.segs_len += 1;
            }
            Keep::Extent(ext) => {
                best_effort(self.extents.release(ext));
            }
        }
        let c = &mut self.conns[slot as usize];
        c.tx_head = (c.tx_head + 1) % self.cfg.sizes.spans;
        c.tx_len -= 1;
        if c.tx_len == 0 {
            self.tx_busy -= 1;
        }
        if matches!(s.keep, Keep::Segment(_)) {
            self.segment_returned();
        }
    }

    /// Drop everything queued on `slot`.
    pub(super) fn drop_tx(&mut self, slot: u32) {
        while self.conns[slot as usize].tx_len > 0 {
            self.pop_span(slot);
        }
        let c = &mut self.conns[slot as usize];
        (c.tx_head, c.full) = (0, false);
        (c.shut, c.shut_sent) = (false, false);
        if self.segs_len > 0 {
            // It may have been told of a segment it never took.
            self.segment_returned();
        }
    }

    /// Send what is queued on `conn`, then shut its write side down: the
    /// peer reads everything and then the end of the stream. Receiving
    /// goes on.
    pub fn shutdown_write(&mut self, d: &mut T::Driver, conn: Conn) -> Result<(), NetError> {
        self.cover_call(conn, Col::Shutdown);
        let slot = self.gens.slot_of(conn).ok_or(NetError::Stale)?;
        if !self.flushable(slot) || self.conns[slot as usize].st == St::Refused {
            return Err(NetError::NotUp);
        }
        self.conns[slot as usize].shut = true;
        self.shut_if_sent(d, slot);
        Ok(())
    }

    /// Shut `slot`'s write side down once its queue is empty.
    pub(super) fn shut_if_sent(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &self.conns[slot as usize];
        if !self.flushable(slot) || !c.shut || c.shut_sent || c.tx_len > 0 || c.sending {
            return;
        }
        if self.t.shutdown_write(d, slot).is_ok() {
            let shut = &mut self.conns[slot as usize];
            (shut.shutting, shut.shut_sent) = (true, true);
        } else {
            self.add_work(slot, W_SHUT);
        }
    }
}
