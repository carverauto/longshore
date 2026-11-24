// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Frames as the caller sees them: lent, held and released.

use super::state::W_READ;
use super::Net;
use crate::event::NetError;
use crate::handle::Conn;
use crate::transport::{Span, Transport};
use longshore_common::io::extents::Extent;
use longshore_common::io::BufId;

/// Where a frame's bytes are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mem {
    /// In a ring buffer, until the `poll` after turn `turn`.
    Ring { buf: BufId, turn: u32 },
    /// In frame memory, under a hold of the caller's.
    Extent(Extent),
}

/// A received frame, or with [`crate::Framing::Bytes`] a run of received
/// bytes: lent to the caller where it was received.
///
/// A frame in frame memory stays until [`Net::release`]. A frame lent from
/// the buffer ring is gone at the next [`Net::poll`] unless [`Net::hold`]
/// moves it into frame memory first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame {
    pub(super) conn: Conn,
    pub(super) mem: Mem,
    pub(super) off: u32,
    pub(super) len: u32,
}

impl Frame {
    /// The connection it arrived on.
    pub const fn conn(&self) -> Conn {
        self.conn
    }

    /// Its length in bytes, header included.
    pub const fn len(&self) -> u32 {
        self.len
    }

    /// Whether it has no bytes.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether it is in frame memory, and so stays until released.
    pub const fn is_held(&self) -> bool {
        matches!(self.mem, Mem::Extent(_))
    }

    /// Its frame memory, for a hold by something outside the `Net` (a file
    /// write); `None` for a frame lent from the buffer ring.
    pub const fn extent(&self) -> Option<Extent> {
        match self.mem {
            Mem::Extent(e) => Some(e),
            Mem::Ring { .. } => None,
        }
    }

    /// Where its bytes lie in the pool, for a file write or a send from
    /// the same memory.
    pub const fn span(&self) -> Span {
        let buf = match self.mem {
            Mem::Extent(e) => e.buf(),
            Mem::Ring { buf, .. } => buf,
        };
        Span::new(buf, self.off, self.len)
    }
}

impl<T: Transport> Net<T> {
    /// Whether `frame`'s bytes are still what they were.
    fn is_live(&self, frame: &Frame) -> bool {
        match frame.mem {
            Mem::Ring { turn, .. } => turn == self.turn,
            Mem::Extent(e) => self.extents.is_live(e),
        }
    }

    /// The bytes of `frame`; `None` once it has been released, or for a
    /// frame lent from the buffer ring after the next `poll`.
    pub fn bytes<'d>(&self, d: &'d mut T::Driver, frame: &Frame) -> Option<&'d [u8]> {
        if !self.is_live(frame) {
            return None;
        }
        let span = frame.span();
        let all: &'d [u8] = self.t.bytes(d, span.buf());
        all.get(span.off() as usize..(span.off() + span.len()) as usize)
    }

    /// Keep `frame` past the next `poll`: a frame lent from the buffer
    /// ring is copied into frame memory and the new frame returned; one
    /// already there is returned as it is. Release what is returned.
    pub fn hold(&mut self, d: &mut T::Driver, frame: Frame) -> Result<Frame, NetError> {
        let Mem::Ring { buf, .. } = frame.mem else {
            return Ok(frame);
        };
        if !self.is_live(&frame) {
            return Err(NetError::Stale);
        }
        let ext = self.extents.take().ok_or(NetError::NoMemory)?;
        let (src, dst) = self.t.bytes_pair(d, buf, ext.buf());
        let (from, n) = (frame.off as usize, frame.len as usize);
        dst[..n].copy_from_slice(&src[from..from + n]);
        self.count_held(frame.conn, frame.len);
        Ok(Frame {
            conn: frame.conn,
            mem: Mem::Extent(ext),
            off: 0,
            len: frame.len,
        })
    }

    /// Done with `frame`. For a frame in frame memory this drops the
    /// caller's hold: the memory is free again when every other hold (a
    /// queued send, a file write) is gone too.
    pub fn release(&mut self, frame: Frame) {
        let Mem::Extent(ext) = frame.mem else {
            return;
        };
        if self.extents.release(ext).is_err() {
            return;
        }
        let Some(slot) = self.gens.slot_of(frame.conn) else {
            return;
        };
        let c = &mut self.conns[slot as usize];
        c.held = c.held.saturating_sub(frame.len);
        if c.paused && c.held < c.opts.held_max {
            c.paused = false;
            self.add_work(slot, W_READ);
        }
    }

    /// Count `len` bytes of frame memory as held by `conn`'s caller.
    pub(super) fn count_held(&mut self, conn: Conn, len: u32) {
        if let Some(slot) = self.gens.slot_of(conn) {
            let c = &mut self.conns[slot as usize];
            c.held = c.held.saturating_add(len);
        }
    }
}
