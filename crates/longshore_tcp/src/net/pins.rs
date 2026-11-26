// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Memory a zero-copy send still reads after it was reported.
//!
//! A zero-copy send is reported as soon as its bytes are taken, so that
//! the connection's next send follows at once, not a round trip later.
//! The transport goes on reading the memory until the peer has
//! acknowledged the bytes. Until then the memory is pinned: a send
//! segment stays out of the pool though its span is gone, frame memory
//! keeps one more hold. A connection has `Sizes::with_zero_copy_sends`
//! pins, and the set a number in all, which is what it asked the backend
//! operations for; with either out it sends with a copy. A slot is not
//! free while it has a pin out: the pins' numbers are the slot's.

use super::cover::Col;
use super::state::{best_effort, Keep, TxSpan, NONE};
use super::Net;
use crate::addr::Addr;
use crate::transport::{Lend, Transport};

/// Whether the kernel copies what is sent to `addr` (for an accepted
/// connection, the address it was accepted at) whatever it is asked: a
/// Unix-domain socket, or loopback. A zero-copy send there only adds the
/// bookkeeping of one, and over loopback carries less than a copied one.
/// A listener at the unspecified address is not known to be either.
pub(super) fn copies_anyway(addr: &Addr) -> bool {
    match addr {
        Addr::Unix(_) => true,
        Addr::Ip(at) => at.ip().is_loopback(),
    }
}

impl<T: Transport> Net<T> {
    /// How the span `s` at the front of `slot`'s queue is to be sent, and
    /// the pin taken for it, or `NONE`.
    pub(super) fn lend_for(&mut self, slot: u32, s: &TxSpan) -> (Lend, u32) {
        let c = &self.conns[slot as usize];
        // Nothing sealed for TLS is sent from where the caller put it.
        if c.copied || c.secure || s.len < self.cfg.sizes.zero_copy_min {
            return (Lend::No, NONE);
        }
        if self.cfg.sizes.zero_copy_sends == 0 {
            return (Lend::UntilSent, NONE);
        }
        match self.pin(slot, s.keep) {
            Some(id) => (Lend::UntilReleased(id), id),
            // Every pin is out: a copy now, not a wait for one.
            None => (Lend::No, NONE),
        }
    }

    /// Pin what `keep` names for a send of `slot`: the pin's number, or
    /// `None` when the connection has none left.
    fn pin(&mut self, slot: u32, keep: Keep) -> Option<u32> {
        let per = self.cfg.sizes.zero_copy_sends;
        let c = &self.conns[slot as usize];
        if c.pins == per || self.pins_out == self.cfg.sizes.lent_most(self.cfg.conns()) {
            return None;
        }
        let first = (slot * per) as usize;
        let id = (0..per)
            .map(|k| (c.pin_next + k) % per)
            .find(|id| self.pins[first + *id as usize].is_none())?;
        match keep {
            Keep::Extent(ext) => self.extents.hold(ext).ok()?,
            Keep::Segment(index) => self.seg_pins[index as usize] += 1,
        }
        self.pins[first + id as usize] = Some(keep);
        let pinned = &mut self.conns[slot as usize];
        (pinned.pins, pinned.pin_next) = (pinned.pins + 1, (id + 1) % per);
        self.pins_out += 1;
        Some(id)
    }

    /// Nothing reads the memory of pin `id` of `slot` any more.
    pub(super) fn unpin(&mut self, slot: u32, id: u32) {
        let at = (slot * self.cfg.sizes.zero_copy_sends + id) as usize;
        let Some(keep) = self.pins.get_mut(at).and_then(Option::take) else {
            return;
        };
        self.pins_out -= 1;
        let c = &mut self.conns[slot as usize];
        c.pins -= 1;
        if c.pins == 0 && c.tx_len == 0 {
            self.tx_busy -= 1;
        }
        match keep {
            Keep::Extent(ext) => best_effort(self.extents.release(ext)),
            Keep::Segment(index) => {
                let i = index as usize;
                self.seg_pins[i] -= 1;
                if self.seg_pins[i] == 0 && core::mem::take(&mut self.seg_parked[i]) {
                    self.segs[self.segs_len] = index;
                    self.segs_len += 1;
                    self.segment_returned();
                }
            }
        }
    }

    /// The span that held segment `index` is gone. Whether the segment is
    /// back in the pool: not while a send still reads it.
    pub(super) fn retire_segment(&mut self, index: u32) -> bool {
        if self.seg_pins[index as usize] > 0 {
            self.seg_parked[index as usize] = true;
            return false;
        }
        self.segs[self.segs_len] = index;
        self.segs_len += 1;
        true
    }

    /// The transport no longer reads the memory of `slot`'s send `id`.
    pub(super) fn on_released(&mut self, slot: u32, id: u32) {
        self.cover(slot, Col::Released);
        self.unpin(slot, id);
        self.try_free(slot);
    }
}
