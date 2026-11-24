// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving from buffer-ring buffers, as an armed receive delivers them.
//!
//! [`Stream::recv`] asks `Io` for bytes itself. With an armed receive the
//! bytes arrive unasked, in buffers the backend picks, and the owner hands
//! each one to [`Stream::on_ring_recv`]. A TLS record that lies whole in a
//! ring buffer is opened from there straight into the destination; only a
//! record that continues in the next ring buffer is carried, in the
//! stream's own TLS receive buffer. No plaintext is ever written to a ring
//! buffer, so the owner gives it back as soon as it has been consumed.

use crate::{Stream, TlsError};
use longshore_common::io::{BufId, Io};
#[cfg(feature = "tls-core")]
use longshore_tls::record::{
    record_len, CONTENT_ALERT, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE, FRAGMENT_MAX,
    HEADER_LEN, TAG_LEN,
};

/// Room a destination needs when it is handed to [`Stream::on_ring_recv`]
/// empty: enough to open the largest record, with either record layer.
pub const RING_DST_MIN: u32 = 16_384 + 1 + 16;

/// Bytes of a ring buffer to consume: `buf[off..off + len]`.
#[derive(Clone, Copy, Debug)]
pub struct RingBytes {
    buf: BufId,
    off: u32,
    len: u32,
}

impl RingBytes {
    /// `len` bytes of `buf` starting at `off`.
    pub const fn new(buf: BufId, off: u32, len: u32) -> RingBytes {
        RingBytes { buf, off, len }
    }
}

/// Where plaintext goes: up to `max` bytes of `buf` from `off`.
#[derive(Clone, Copy, Debug)]
pub struct RingDst {
    buf: BufId,
    off: u32,
    max: u32,
}

impl RingDst {
    /// Up to `max` bytes of `buf` starting at `off`.
    pub const fn new(buf: BufId, off: u32, max: u32) -> RingDst {
        RingDst { buf, off, max }
    }
}

/// What one call to [`Stream::on_ring_recv`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RingStep {
    consumed: u32,
    produced: u32,
    closed: bool,
    no_room: bool,
}

impl RingStep {
    /// Bytes of the ring buffer taken.
    pub const fn consumed(self) -> u32 {
        self.consumed
    }

    /// The destination has no room for the next record: call again with
    /// the rest of the ring buffer and a new destination. The record may
    /// be one the stream keeps whole, with every byte of the ring buffer
    /// taken: the next call then has no bytes to offer.
    pub const fn no_room(self) -> bool {
        self.no_room
    }

    /// Bytes of plaintext written to the destination.
    pub const fn produced(self) -> u32 {
        self.produced
    }

    /// The peer ended the stream in order (a TLS close alert). Nothing
    /// after it is consumed.
    pub const fn closed(self) -> bool {
        self.closed
    }
}

impl Stream {
    /// Take received bytes from a ring buffer and write their plaintext to
    /// `dst`.
    ///
    /// On a plain stream the bytes are the plaintext and are copied; an
    /// owner that wants no copy reads the ring buffer in place instead. On
    /// a TLS stream each whole record is opened from the ring buffer into
    /// `dst`, and the part of a record that the ring buffer ends in is
    /// kept for the next call.
    ///
    /// A record is opened only when `dst` has room for all of it, so a
    /// destination handed in empty must hold [`RING_DST_MIN`] bytes. Do not
    /// mix this with [`Stream::recv`] on one connection, except that it may
    /// follow a handshake: call it once with no bytes to take what arrived
    /// behind the handshake.
    pub fn on_ring_recv<IO: Io>(
        &mut self,
        io: &mut IO,
        from: RingBytes,
        dst: RingDst,
    ) -> Result<RingStep, TlsError> {
        #[cfg(feature = "tls-core")]
        if let Some(t) = self.tls.as_mut().filter(|t| t.active) {
            return t.ring_recv(io, from, dst);
        }
        let n = from.len.min(dst.max);
        if n > 0 {
            let (src, out) = io.buf_pair(from.buf, dst.buf);
            out[dst.off as usize..(dst.off + n) as usize]
                .copy_from_slice(&src[from.off as usize..(from.off + n) as usize]);
        }
        Ok(RingStep {
            consumed: n,
            produced: n,
            closed: false,
            no_room: n < from.len,
        })
    }
}

/// What opening one record came to.
#[cfg(feature = "tls-core")]
enum Opened {
    /// The destination has no room for it.
    NoRoom,
    /// Opened; this many bytes of application data were produced.
    Data(u32),
    /// A close alert.
    Closed,
}

#[cfg(feature = "tls-core")]
impl crate::stream::Tls {
    fn ring_recv<IO: Io>(
        &mut self,
        io: &mut IO,
        from: RingBytes,
        dst: RingDst,
    ) -> Result<RingStep, TlsError> {
        debug_assert!(self.spill_len == 0, "recv and on_ring_recv were mixed");
        let mut step = RingStep {
            consumed: 0,
            produced: 0,
            closed: self.closed,
            no_room: false,
        };
        // Records carried from earlier buffers come first.
        while !step.closed && self.rx_end > self.rx_start {
            let have = (self.rx_end - self.rx_start) as usize;
            let need = self.carried_need(io, have)?;
            if need > 0 {
                let take = need.min((from.len - step.consumed) as usize);
                if take == 0 {
                    return Ok(step);
                }
                self.carry(io, from.buf, (from.off + step.consumed) as usize, take)?;
                step.consumed += take as u32;
                continue;
            }
            let len = self.carried_len(io)?;
            let at = self.rx_start as usize;
            match self.open_one(io, (self.rx_buf, at, len), dst, step.produced)? {
                Opened::NoRoom => {
                    step.no_room = true;
                    return Ok(step);
                }
                Opened::Data(n) => step.produced += n,
                Opened::Closed => step.closed = true,
            }
            self.rx_start += len as u32;
        }
        if self.rx_start == self.rx_end {
            (self.rx_start, self.rx_end) = (0, 0);
        }
        // Then whole records straight from the ring buffer.
        while !step.closed && step.consumed < from.len {
            let at = (from.off + step.consumed) as usize;
            let rest = (from.len - step.consumed) as usize;
            let whole = record_len(&io.buf(from.buf)[at..at + rest])
                .map_err(|_| TlsError::BadRecord)?
                .filter(|len| *len <= rest);
            let Some(len) = whole else {
                // The record continues in the next ring buffer: carry it.
                let (src, rx) = io.buf_pair(from.buf, self.rx_buf);
                rx[..rest].copy_from_slice(&src[at..at + rest]);
                (self.rx_start, self.rx_end) = (0, rest as u32);
                step.consumed = from.len;
                break;
            };
            match self.open_one(io, (from.buf, at, len), dst, step.produced)? {
                Opened::NoRoom => {
                    step.no_room = true;
                    break;
                }
                Opened::Data(n) => step.produced += n,
                Opened::Closed => step.closed = true,
            }
            step.consumed += len as u32;
        }
        self.closed = step.closed;
        Ok(step)
    }

    /// Add `take` bytes of `buf` from `at` to the carried bytes. What
    /// arrived behind a handshake is carried wherever the handshake left
    /// it, so it is moved to the front when the bytes do not fit behind
    /// it. A record that does not fit even then is larger than the buffer.
    fn carry<IO: Io>(
        &mut self,
        io: &mut IO,
        buf: BufId,
        at: usize,
        take: usize,
    ) -> Result<(), TlsError> {
        let (start, have) = (
            self.rx_start as usize,
            (self.rx_end - self.rx_start) as usize,
        );
        let (src, rx) = io.buf_pair(buf, self.rx_buf);
        let cap = rx.len().min(self.cap as usize);
        if have + take > cap {
            return Err(TlsError::BadRecord);
        }
        let front = if start + have + take > cap {
            rx.copy_within(start..start + have, 0);
            0
        } else {
            start
        };
        let end = front + have;
        rx[end..end + take].copy_from_slice(&src[at..at + take]);
        (self.rx_start, self.rx_end) = (front as u32, (end + take) as u32);
        Ok(())
    }

    /// Bytes still missing from the carried record: the rest of its header,
    /// then the rest of the record. Zero when it is whole.
    fn carried_need<IO: Io>(&self, io: &mut IO, have: usize) -> Result<usize, TlsError> {
        if have < HEADER_LEN {
            return Ok(HEADER_LEN - have);
        }
        Ok(self.carried_len(io)?.saturating_sub(have))
    }

    /// Length of the carried record, whose header is all there.
    fn carried_len<IO: Io>(&self, io: &mut IO) -> Result<usize, TlsError> {
        let carried = &io.buf(self.rx_buf)[self.rx_start as usize..self.rx_end as usize];
        record_len(carried)
            .map_err(|_| TlsError::BadRecord)?
            .ok_or(TlsError::BadRecord)
    }

    /// Open the record at `buf[at..at + len]` into what is left of `dst`
    /// after `produced` bytes.
    fn open_one<IO: Io>(
        &mut self,
        io: &mut IO,
        (buf, at, len): (BufId, usize, usize),
        dst: RingDst,
        produced: u32,
    ) -> Result<Opened, TlsError> {
        debug_assert!(len <= HEADER_LEN + FRAGMENT_MAX + 256 + TAG_LEN);
        let needed = self.rx.open_room(len);
        let space = (dst.max - produced) as usize;
        if needed > space {
            return Ok(Opened::NoRoom);
        }
        let to = (dst.off + produced) as usize;
        let (src, out) = io.buf_pair(buf, dst.buf);
        let (ctype, n) = self
            .rx
            .open(&src[at..at + len], &mut out[to..to + needed])
            .map_err(|_| TlsError::Record)?;
        match ctype {
            CONTENT_APPLICATION_DATA => Ok(Opened::Data(n as u32)),
            // Session tickets after the handshake: nothing to do. A peer's
            // KeyUpdate also lands here and is not followed: its next
            // record fails authentication and the connection is dropped.
            CONTENT_HANDSHAKE => Ok(Opened::Data(0)),
            // An alert is two bytes, level then description; close_notify
            // is an orderly end of stream, any other alert a failure.
            CONTENT_ALERT if n == 2 && out[to + 1] == 0 => Ok(Opened::Closed),
            CONTENT_ALERT if n == 2 => Err(TlsError::Alert(out[to + 1])),
            _ => Err(TlsError::BadRecord),
        }
    }
}
