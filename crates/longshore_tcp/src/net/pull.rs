// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Cutting received bytes into frames as the caller pulls them.
//!
//! A ring connection's input is its backlog (bytes moved out of ring
//! buffers the caller left unpulled over a `poll`), then its chunks, in
//! order. A frame lying whole in one of them is lent where it is. A frame
//! that continues in the next is put together in frame memory, as is a
//! header that arrives in pieces.

use super::frames::{Frame, Mem};
use super::state::{best_effort, St, EV_READABLE, NONE, W_READ};
use super::Net;
use crate::config::Rx;
use crate::event::{NetError, Why};
use crate::framing::Framing;
use crate::handle::Conn;
use crate::transport::Transport;
use longshore_common::io::BufId;

/// The input a ring connection reads next.
#[derive(Clone, Copy)]
pub(super) struct Source {
    pub(super) buf: BufId,
    pub(super) off: u32,
    pub(super) avail: u32,
    backlog: bool,
}

impl<T: Transport> Net<T> {
    /// The next frame received on `conn`, or `None` when there is none for
    /// now. Pull until `None` after each [`crate::Event::Readable`].
    pub fn next_frame(&mut self, d: &mut T::Driver, conn: Conn) -> Result<Option<Frame>, NetError> {
        let slot = self.gens.slot_of(conn).ok_or(NetError::Stale)?;
        if !matches!(self.conns[slot as usize].st, St::Up | St::Dying) {
            return Ok(None);
        }
        loop {
            if let Some((ext, len)) = self.conns[slot as usize].ready.take() {
                return Ok(Some(self.deliver(d, slot, Mem::Extent(ext), 0, len)));
            }
            let Some(src) = self.source(slot) else {
                if self.header_whole(slot) {
                    // The input ended with a header: its frame may be whole.
                    if self.begin_frame(d, slot) {
                        continue;
                    }
                    let _stop: Cut = self.out_of_memory(d, slot);
                    return Ok(None);
                }
                // Protected input is opened as frame memory has room.
                if self.conns[slot as usize].secure && self.open_ring(d, slot, false) {
                    continue;
                }
                return Ok(None);
            };
            match self.cut(d, slot, src) {
                Cut::Frame(len) => {
                    let Some(mem) = self.lend(slot, src) else {
                        // The backlog has as many holds as can be counted.
                        self.add_work(slot, W_READ);
                        return Ok(None);
                    };
                    self.advance(slot, src, len);
                    return Ok(Some(self.deliver(d, slot, mem, src.off, len)));
                }
                Cut::Taken(n) => self.advance(slot, src, n),
                Cut::Stop => return Ok(None),
            }
        }
    }

    /// Hand a frame to the caller, counting frame memory against the
    /// connection's budget and reading on where that is due.
    fn deliver(&mut self, d: &mut T::Driver, slot: u32, mem: Mem, off: u32, len: u32) -> Frame {
        let conn = self.gens.conn(slot);
        if matches!(mem, Mem::Extent(_)) {
            let c = &mut self.conns[slot as usize];
            c.held = c.held.saturating_add(len);
            if c.held >= c.opts.held_max {
                self.pause(d, slot);
            }
        }
        if self.conns[slot as usize].opts.rx == Rx::Direct {
            self.start_reading(d, slot);
        }
        Frame {
            conn,
            mem,
            off,
            len,
        }
    }

    /// Whether `slot` has received bytes the caller has not pulled.
    pub(super) fn has_input(&self, slot: u32) -> bool {
        let c = &self.conns[slot as usize];
        c.ready.is_some() || c.unopened || self.source(slot).is_some() || self.header_whole(slot)
    }

    /// Whether a ring connection's header area holds a whole header whose
    /// frame has not been started.
    fn header_whole(&self, slot: u32) -> bool {
        let c = &self.conns[slot as usize];
        match c.opts.framing {
            Framing::Header(h) => {
                c.opts.rx == Rx::Ring && c.asm.is_none() && c.hdr_have == h.header_len() as u32
            }
            Framing::Bytes => false,
        }
    }

    fn source(&self, slot: u32) -> Option<Source> {
        let c = &self.conns[slot as usize];
        if let Some(ext) = c.backlog.filter(|_| c.backlog_len > c.backlog_off) {
            return Some(Source {
                buf: ext.buf(),
                off: c.backlog_off,
                avail: c.backlog_len - c.backlog_off,
                backlog: true,
            });
        }
        // A protected connection's ring buffers hold records, not frames.
        if c.secure {
            return None;
        }
        self.front_chunk(slot)
    }

    /// The first ring buffer `slot` has not finished with, if any.
    pub(super) fn front_chunk(&self, slot: u32) -> Option<Source> {
        let head = self.conns[slot as usize].chunk_head;
        (head != NONE).then(|| {
            let chunk = &self.chunks[head as usize];
            Source {
                buf: BufId::new(self.cfg.ring_first.get() + head),
                off: chunk.off,
                avail: chunk.len - chunk.off,
                backlog: false,
            }
        })
    }

    /// Memory for a frame lent from `src`: the ring buffer itself for this
    /// turn, or one more hold on the backlog.
    fn lend(&mut self, slot: u32, src: Source) -> Option<Mem> {
        if !src.backlog {
            return Some(Mem::Ring {
                buf: src.buf,
                turn: self.turn,
            });
        }
        let ext = self.conns[slot as usize].backlog?;
        self.extents.hold(ext).ok().map(|()| Mem::Extent(ext))
    }

    /// `n` bytes of `src` are dealt with.
    pub(super) fn advance(&mut self, slot: u32, src: Source, n: u32) {
        let c = &mut self.conns[slot as usize];
        if src.backlog {
            c.backlog_off += n;
            if c.backlog_off == c.backlog_len {
                // Frames lent from it keep their own holds.
                if let Some(ext) = c.backlog.take() {
                    best_effort(self.extents.release(ext));
                }
            }
            return;
        }
        let i = c.chunk_head;
        let chunk = &mut self.chunks[i as usize];
        chunk.off += n;
        if chunk.off == chunk.len {
            c.chunk_head = chunk.next;
            if c.chunk_head == NONE {
                c.chunk_tail = NONE;
            }
            // Frames lent from it are good until the next `poll`.
            self.returns[self.returns_len] = src.buf;
            self.returns_len += 1;
        }
    }

    /// Decide what the bytes at the front of `src` are.
    fn cut(&mut self, d: &mut T::Driver, slot: u32, src: Source) -> Cut {
        let c = &self.conns[slot as usize];
        let Framing::Header(h) = c.opts.framing else {
            return Cut::Frame(src.avail);
        };
        let hl = h.header_len() as u32;
        if let Some(ext) = c.asm {
            // The rest of a frame being put together.
            let take = (c.asm_len - c.asm_have).min(src.avail);
            let (from, to) = self.t.bytes_pair(d, src.buf, ext.buf());
            let (at, have) = (src.off as usize, c.asm_have as usize);
            to[have..have + take as usize].copy_from_slice(&from[at..at + take as usize]);
            let filling = &mut self.conns[slot as usize];
            filling.asm_have += take;
            if filling.asm_have == filling.asm_len {
                (filling.asm, filling.ready) = (None, Some((ext, filling.asm_len)));
            }
            return Cut::Taken(take);
        }
        if c.hdr_have == hl {
            // A whole header waiting for frame memory.
            return if self.begin_frame(d, slot) {
                Cut::Taken(0)
            } else {
                self.out_of_memory(d, slot)
            };
        }
        if c.hdr_have > 0 || src.avail < hl {
            // A header in pieces: collect it in the header area.
            let take = (hl - c.hdr_have).min(src.avail);
            let (hbuf, hoff) = self.header_area(slot);
            let (from, to) = self.t.bytes_pair(d, src.buf, hbuf);
            let (at, have) = (src.off as usize, (hoff + c.hdr_have) as usize);
            to[have..have + take as usize].copy_from_slice(&from[at..at + take as usize]);
            self.conns[slot as usize].hdr_have += take;
            return Cut::Taken(take);
        }
        let header = &self.t.bytes(d, src.buf)[src.off as usize..(src.off + hl) as usize];
        let Ok(frame_len) = h.frame_len(header) else {
            self.end(slot, Why::Protocol);
            self.drop_input(slot);
            return Cut::Stop;
        };
        if frame_len as u32 <= src.avail {
            return Cut::Frame(frame_len as u32);
        }
        // It continues in the next chunk: start putting it together.
        let Some(ext) = self.extents.take() else {
            return self.out_of_memory(d, slot);
        };
        let (from, to) = self.t.bytes_pair(d, src.buf, ext.buf());
        let (at, n) = (src.off as usize, src.avail as usize);
        to[..n].copy_from_slice(&from[at..at + n]);
        let starting = &mut self.conns[slot as usize];
        (starting.asm, starting.asm_have, starting.asm_len) =
            (Some(ext), src.avail, frame_len as u32);
        Cut::Taken(src.avail)
    }

    /// No frame memory: stop reading, and try again on the next `poll`.
    fn out_of_memory(&mut self, d: &mut T::Driver, slot: u32) -> Cut {
        if self.conns[slot as usize].st == St::Up {
            self.pause(d, slot);
            self.conns[slot as usize].paused = false;
            self.add_work(slot, W_READ);
        }
        Cut::Stop
    }

    /// Forget what `slot` received and did not deliver: its framing broke.
    pub(super) fn drop_input(&mut self, slot: u32) {
        self.drop_chunks(slot);
        self.drop_rx(slot);
    }

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
                    if self.open_ring(d, slot, false) && !had {
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
}

/// What the front of a source turned out to be.
enum Cut {
    /// A whole frame of this many bytes, lying in the source.
    Frame(u32),
    /// This many bytes were moved elsewhere; look again.
    Taken(u32),
    /// Nothing more can be pulled for now.
    Stop,
}
