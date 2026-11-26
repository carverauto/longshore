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

use super::cover::Col;
use super::frames::{Frame, Mem};
use super::state::{best_effort, St, NONE, W_READ};
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
    pub(super) backlog: bool,
}

impl<T: Transport> Net<T> {
    /// The next frame received on `conn`, or `None` when there is none for
    /// now. Pull until `None` after each [`crate::Event::Readable`].
    pub fn next_frame(&mut self, d: &mut T::Driver, conn: Conn) -> Result<Option<Frame>, NetError> {
        self.cover_call(conn, Col::NextFrame);
        let slot = self.gens.slot_of(conn).ok_or(NetError::Stale)?;
        if !matches!(self.conns[slot as usize].st, St::Up | St::Finishing) {
            return Ok(None);
        }
        let frame = self.pull(d, slot);
        if self.conns[slot as usize].st == St::Finishing {
            if !self.has_input(slot) {
                // That was the last of it: now the end can be told.
                self.settle_end(slot);
            } else if frame.is_none() {
                // Input that cannot be delivered for now, for want of
                // frame memory or of a hold: offered again on `poll`.
                self.add_work(slot, W_READ);
            }
        }
        Ok(frame)
    }

    /// The next frame of `slot`, if one can be cut and lent right now.
    fn pull(&mut self, d: &mut T::Driver, slot: u32) -> Option<Frame> {
        loop {
            let run = core::mem::replace(&mut self.conns[slot as usize].ready_run, NONE);
            if run != NONE {
                let len = self.zc.deliver(run);
                return Some(self.deliver(d, slot, Mem::Pages(self.zc.run(run)), 0, len));
            }
            if let Some((ext, len)) = self.conns[slot as usize].ready.take() {
                let lead = self.lead(slot);
                return Some(self.deliver(d, slot, Mem::Extent(ext), lead, len));
            }
            let Some(src) = self.source(slot) else {
                if self.header_whole(slot) {
                    // The input ended with a header: its frame may be whole.
                    if self.begin_frame(d, slot) {
                        continue;
                    }
                    let _stop: Cut = self.out_of_memory(d, slot);
                    return None;
                }
                // Protected input is opened as frame memory has room.
                if self.conns[slot as usize].secure && self.open_ring(d, slot) {
                    continue;
                }
                return None;
            };
            match self.cut(d, slot, src) {
                Cut::Frame(len) => {
                    let Some(mem) = self.lend(slot, src) else {
                        // The backlog has as many holds as can be counted.
                        self.add_work(slot, W_READ);
                        return None;
                    };
                    self.advance(slot, src, len);
                    return Some(self.deliver(d, slot, mem, src.off, len));
                }
                Cut::Taken(n) => self.advance(slot, src, n),
                Cut::Stop => return None,
            }
        }
    }

    /// Hand a frame to the caller, counting frame memory against the
    /// connection's budget and reading on where that is due.
    fn deliver(&mut self, d: &mut T::Driver, slot: u32, mem: Mem, off: u32, len: u32) -> Frame {
        let conn = self.gens.conn(slot);
        if matches!(mem, Mem::Extent(_) | Mem::Pages(_)) {
            let c = &mut self.conns[slot as usize];
            c.held = c.held.saturating_add(len);
            if c.held >= c.opts.held_max {
                self.pause(d, slot);
            }
        }
        if matches!(self.conns[slot as usize].opts.rx, Rx::Direct | Rx::ZeroCopy) {
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
        c.ready.is_some()
            || c.ready_run != NONE
            || self.source(slot).is_some()
            || self.header_whole(slot)
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

    pub(super) fn source(&self, slot: u32) -> Option<Source> {
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
        if c.asm.is_some() {
            // The rest of a frame being put together.
            return Cut::Taken(self.fill_assembly(d, slot, src));
        }
        if c.hdr_have == hl {
            // A whole header, collected in pieces. What follows it in the
            // backlog can take it back in front; otherwise its frame
            // waits for frame memory of its own.
            if src.backlog && self.header_into_backlog(d, slot, src) {
                return Cut::Taken(0);
            }
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
            self.broke_framing(slot);
            return Cut::Stop;
        };
        if frame_len as u32 <= src.avail {
            return Cut::Frame(frame_len as u32);
        }
        // It continues in the next chunk: start putting it together, in
        // the backlog's own buffer where that is free to be used.
        if src.backlog && self.assemble_in_backlog(d, slot, src, frame_len as u32) {
            return Cut::Taken(0);
        }
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

    /// What `slot` received is no frame. It ends for that, even when its
    /// stream had ended already: an end is in order only between frames.
    /// What is left of its input is dropped.
    pub(super) fn broke_framing(&mut self, slot: u32) {
        if self.conns[slot as usize].st == St::Finishing {
            return self.fail_finishing(slot, Why::Protocol, true);
        }
        self.drop_input(slot);
        self.end(slot, Why::Protocol);
    }

    /// Forget what `slot` received and did not deliver.
    pub(super) fn drop_input(&mut self, slot: u32) {
        self.drop_chunks(slot);
        self.drop_rx(slot);
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
