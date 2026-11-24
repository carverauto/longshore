// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Moving unpulled input out of the buffer ring, and keeping that from
//! tying up frame memory.
//!
//! A connection needs at most one buffer of frame memory for input it
//! has received and not delivered: its backlog, or the frame it is
//! putting together, or a whole frame that waits to be pulled. Were it to
//! need two at once (a backlog and a second buffer for a frame that
//! continues past it), as many connections as there are buffers could
//! each hold one and wait for another for ever.

use super::pull::Source;
use super::state::{EV_READABLE, W_READ};
use super::Net;
use crate::transport::Transport;

impl<T: Transport> Net<T> {
    /// Before ring buffers go back: move what the caller left unpulled out
    /// of them, into frame memory, so that one connection's unread input
    /// cannot keep the shared ring from refilling.
    pub(super) fn spill_unpulled(&mut self, d: &mut T::Driver) {
        let pending = core::mem::take(&mut self.ring_pending_len);
        for i in 0..pending {
            let slot = self.ring_pending[i];
            self.conns[slot as usize].ring_listed = false;
            while let Some(src) = self.front_chunk(slot) {
                let moved = if self.conns[slot as usize].secure {
                    let had = self.source(slot).is_some();
                    if self.open_ring(d, slot) && !had {
                        self.queue_event(slot, EV_READABLE);
                    }
                    self.front_chunk(slot).is_none()
                } else {
                    self.spill_chunk(d, slot, src)
                };
                if !moved {
                    // No room for more: it keeps these buffers and reads
                    // no further until it is pulled.
                    self.pause(d, slot);
                    self.conns[slot as usize].ring_listed = true;
                    self.ring_pending[self.ring_pending_len] = slot;
                    self.ring_pending_len += 1;
                    break;
                }
            }
            let c = &mut self.conns[slot as usize];
            if !c.ring_listed && c.paused && c.held < c.opts.held_max {
                // It keeps no ring buffer any more: read on.
                c.paused = false;
                self.add_work(slot, W_READ);
            }
        }
    }

    /// Move the front chunk of `slot` to its backlog; `false` when the
    /// backlog cannot take all of it.
    fn spill_chunk(&mut self, d: &mut T::Driver, slot: u32, src: Source) -> bool {
        if self.feed_assembly(d, slot, src) {
            return true;
        }
        let size = self.cfg.sizes.frame_size;
        let c = &mut self.conns[slot as usize];
        // A backlog being read from the front is not appended to: the
        // caller may hold frames lent from it.
        let ext = match c.backlog {
            Some(ext) if c.backlog_off == 0 => ext,
            Some(_) => return false,
            None => {
                let Some(ext) = self.extents.take() else {
                    return false;
                };
                (c.backlog, c.backlog_off, c.backlog_len) = (Some(ext), 0, 0);
                ext
            }
        };
        let have = self.conns[slot as usize].backlog_len;
        if src.avail > size - have {
            return false;
        }
        let (from, to) = self.t.bytes_pair(d, src.buf, ext.buf());
        let (at, n) = (src.off as usize, src.avail as usize);
        to[have as usize..have as usize + n].copy_from_slice(&from[at..at + n]);
        self.conns[slot as usize].backlog_len += src.avail;
        // The chunk is the front of the chunk list, whatever `source` says
        // now that there is a backlog.
        self.advance(slot, src, src.avail);
        true
    }

    /// A frame of `slot` is being put together and nothing lies before
    /// the chunk `src` (no backlog): its bytes go into that frame, as far
    /// as it needs them, and take no further frame memory.
    fn feed_assembly(&mut self, d: &mut T::Driver, slot: u32, src: Source) -> bool {
        let c = &self.conns[slot as usize];
        let (Some(ext), None) = (c.asm, c.backlog) else {
            return false;
        };
        let take = (c.asm_len - c.asm_have).min(src.avail);
        let (from, to) = self.t.bytes_pair(d, src.buf, ext.buf());
        let (at, have) = (src.off as usize, c.asm_have as usize);
        to[have..have + take as usize].copy_from_slice(&from[at..at + take as usize]);
        let filling = &mut self.conns[slot as usize];
        filling.asm_have += take;
        if filling.asm_have == filling.asm_len {
            (filling.asm, filling.ready) = (None, Some((ext, filling.asm_len)));
            self.queue_event(slot, EV_READABLE);
        }
        self.advance(slot, src, take);
        // A whole frame waits now: what follows it needs a backlog.
        take > 0
    }

    /// The frame at the end of `slot`'s backlog continues beyond it. When
    /// nothing else holds the backlog's buffer (no frame lent from it is
    /// still with the caller), that buffer is where the frame is put
    /// together: its part moves to the front and no second buffer is
    /// needed. `false` when a lent frame still holds the buffer.
    pub(super) fn assemble_in_backlog(
        &mut self,
        d: &mut T::Driver,
        slot: u32,
        src: Source,
        frame_len: u32,
    ) -> bool {
        let Some(ext) = self.conns[slot as usize].backlog else {
            return false;
        };
        if self.extents.holds(ext) != Some(1) {
            return false;
        }
        let (from, to) = (src.off as usize, (src.off + src.avail) as usize);
        self.t.bytes(d, ext.buf()).copy_within(from..to, 0);
        let c = &mut self.conns[slot as usize];
        (c.backlog, c.backlog_off, c.backlog_len) = (None, 0, 0);
        (c.asm, c.asm_have, c.asm_len) = (Some(ext), src.avail, frame_len);
        true
    }
}
