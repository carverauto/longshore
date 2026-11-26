// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The seam between `Net` and whatever moves bytes.
//!
//! `Net` holds everything that does not depend on how bytes reach the wire:
//! handles, framing, frames and their holds, send queues, deadlines,
//! reconnecting, accept policy. A [`Transport`] moves bytes for numbered
//! connection and listener slots and says what happened. It borrows its
//! [`Transport::Driver`] on every call and owns none, so the thread's other
//! work (files, other clients) shares the driver.
//!
//! The trait speaks of slots and of pool buffers ([`BufId`]): the pool is
//! the one thing every transport shares. It names no operation, socket or
//! token type of a particular backend.

use crate::addr::Addr;
use crate::config::Config;
use longshore_common::io::BufId;
use std::net::SocketAddr;

mod happened;
pub use happened::{Happened, Lend};

/// Why a transport did not start something.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The driver has no room for another operation right now. `Net` asks
    /// again on its next `poll`.
    Busy,
    /// The transport cannot do this at all.
    Unsupported,
}

/// The result a transport reports when TLS failed: a handshake that did
/// not complete, a record that did not authenticate, an alert from the
/// peer. (`EPROTO`, negated.)
pub const TLS_FAILED: i32 = -71;

/// Which side of a TLS handshake a connection takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role<'a> {
    /// The side that connected; the peer's certificate must match
    /// `server_name`.
    Client {
        /// The name the peer's certificate must match.
        server_name: &'a str,
    },
    /// The side that accepted.
    Server,
}

/// What [`Transport::open`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Opened {
    consumed: u32,
    produced: u32,
    closed: bool,
    no_room: bool,
}

impl Opened {
    /// `consumed` bytes taken, `produced` bytes of plaintext written, and
    /// whether the peer ended the stream in order.
    pub const fn new(consumed: u32, produced: u32, closed: bool) -> Opened {
        Opened {
            consumed,
            produced,
            closed,
            no_room: false,
        }
    }

    /// The same, with whether the destination had no room for the next
    /// record.
    #[must_use]
    pub const fn with_no_room(mut self, no_room: bool) -> Opened {
        self.no_room = no_room;
        self
    }

    /// Bytes taken from the ring buffer.
    pub const fn consumed(self) -> u32 {
        self.consumed
    }

    /// The destination has no room for the next record: one in the bytes
    /// not taken, or one the transport keeps whole. `Net` asks again with
    /// a new destination, and with no bytes when all were taken.
    pub const fn no_room(self) -> bool {
        self.no_room
    }

    /// Bytes of plaintext written to the destination.
    pub const fn produced(self) -> u32 {
        self.produced
    }

    /// The peer ended the stream in order; nothing after it is taken.
    pub const fn closed(self) -> bool {
        self.closed
    }
}

/// How a listener listens, as far as the transport is concerned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Listening {
    reuse_port: bool,
    backlog: u32,
}

impl Listening {
    /// Share the port when `reuse_port`; let `backlog` connections wait.
    pub const fn new(reuse_port: bool, backlog: u32) -> Listening {
        Listening {
            reuse_port,
            backlog,
        }
    }

    /// Whether the port is shared with other listeners.
    pub const fn reuse_port(self) -> bool {
        self.reuse_port
    }

    /// Connections allowed to wait to be accepted.
    pub const fn backlog(self) -> u32 {
        self.backlog
    }
}

/// Part of a pool buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    buf: BufId,
    off: u32,
    len: u32,
}

impl Span {
    /// `len` bytes of `buf` from `off`.
    pub const fn new(buf: BufId, off: u32, len: u32) -> Span {
        Span { buf, off, len }
    }

    /// The buffer.
    pub const fn buf(self) -> BufId {
        self.buf
    }

    /// Where the bytes start.
    pub const fn off(self) -> u32 {
        self.off
    }

    /// How many bytes.
    pub const fn len(self) -> u32 {
        self.len
    }

    /// Whether it holds no bytes.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Something that moves bytes for `Net`.
pub trait Transport {
    /// What the transport runs on; the caller owns it and lends it.
    type Driver;
    /// What the driver reports when an operation ends.
    type Completion: Copy;

    /// Called once by `Net::new`, with the configuration resolved against
    /// the backend's final `Limits`.
    fn attach(&mut self, config: &Config);
    /// The driver's clock, in nanoseconds: the same clock the caller reads
    /// for `Net::poll`.
    fn now_ns(&self, d: &Self::Driver) -> u64;
    /// The bytes of a pool buffer.
    fn bytes<'d>(&self, d: &'d mut Self::Driver, buf: BufId) -> &'d mut [u8];
    /// The bytes of two different pool buffers at once.
    fn bytes_pair<'d>(
        &self,
        d: &'d mut Self::Driver,
        a: BufId,
        b: BufId,
    ) -> (&'d mut [u8], &'d mut [u8]);

    /// Bind and listen in listener slot `listener`; the bound address, for
    /// an IP address.
    fn listen(
        &mut self,
        d: &mut Self::Driver,
        listener: u32,
        addr: &Addr,
        how: Listening,
    ) -> std::io::Result<Option<SocketAddr>>;
    /// Accept connections on `listener` until told otherwise, each into a
    /// slot of the transport's choosing from the accept range.
    fn accept_armed(&mut self, d: &mut Self::Driver, listener: u32) -> Result<(), Refused>;
    /// Accept one connection on `listener` into `slot`, keeping its peer's
    /// address.
    fn accept_into(
        &mut self,
        d: &mut Self::Driver,
        listener: u32,
        slot: u32,
    ) -> Result<(), Refused>;
    /// Connect `slot` to `addr`. `scratch` is pool memory the transport may
    /// use until the connect ends (a Unix path has to lie in the pool).
    fn connect(
        &mut self,
        d: &mut Self::Driver,
        slot: u32,
        addr: &Addr,
        scratch: Span,
    ) -> Result<(), Refused>;
    /// Whether [`Transport::secure`] can work for the connecting side
    /// (`client`) or the accepting side.
    fn can_secure(&self, client: bool) -> bool;
    /// Start a TLS handshake on the connected `slot`; it ends with
    /// [`Happened::Secured`]. From then on what is sent and received on
    /// `slot` is protected.
    fn secure(&mut self, d: &mut Self::Driver, slot: u32, role: Role<'_>) -> Result<(), Refused>;
    /// Turn `from`, bytes an armed receive delivered on the protected
    /// `slot`, into plaintext in `into`. `None` when they do not
    /// authenticate; the connection is then of no further use. With `from`
    /// empty, takes what arrived behind the handshake.
    fn open(&mut self, d: &mut Self::Driver, slot: u32, from: Span, into: Span) -> Option<Opened>;
    /// Receive on `slot` from the buffer ring until told otherwise.
    fn recv_armed(&mut self, d: &mut Self::Driver, slot: u32) -> Result<(), Refused>;
    /// Receive on `slot` once, into `into`. `Some` result when bytes were
    /// there already and it ended at once: no [`Happened::Received`]
    /// follows then.
    fn recv_into(
        &mut self,
        d: &mut Self::Driver,
        slot: u32,
        into: Span,
    ) -> Result<Option<i32>, Refused>;
    /// Receive once on the protected `slot` into memory of the transport's
    /// own: for when its armed receive ended because the ring had no
    /// buffer. `true` when a receive was started. It ends with
    /// [`Happened::Received`]: the bytes it took, 0 at the end of the
    /// stream, or a negated error; what it took is opened with
    /// [`Transport::open`] and no bytes. `false` when the transport holds
    /// a whole record that waits to be opened. A transport that cannot do
    /// this refuses as [`Refused::Unsupported`], and `Net` waits for the
    /// ring instead.
    fn recv_kept(&mut self, d: &mut Self::Driver, slot: u32) -> Result<bool, Refused>;
    /// Receive on `slot` straight into pages of the backend's receive
    /// area, at most `credit` bytes, which is not 0: each run of bytes is
    /// a [`Happened::Piece`], and the receive ends with
    /// [`Happened::RecvEnded`]: 0 when the credit is used or the stream
    /// ended first, which `Net` tells apart by the bytes it was given; or
    /// a negated error, `-ENOSPC` and `-ENOMEM` among them, after which
    /// the rest is asked for again. A transport without a receive area
    /// ends it with `-ENOBUFS` at once.
    fn recv_zc(&mut self, d: &mut Self::Driver, slot: u32, credit: u32) -> Result<(), Refused>;
    /// Return a piece a [`Happened::Piece`] named: once, as it was given.
    fn give_back_piece(&mut self, d: &mut Self::Driver, page: BufId, off: u32, len: u32);
    /// Whether the driver found no room to report in since this was last
    /// asked, in any number of its turns: a zero-copy receive with credit
    /// left may then stay silent though bytes wait for it, until it is
    /// stopped and started again. Asking clears it.
    fn take_ring_full(&self, d: &mut Self::Driver) -> bool;
    /// Mark every send on `slot` from now on as the end of one of the
    /// kernel's buffers (`MSG_EOR`), so that the next send begins a packet
    /// of its own; until the slot's connection ends. A transport that has
    /// no such thing does nothing.
    fn end_sends(&mut self, _d: &mut Self::Driver, _slot: u32) {}
    /// Stop the receive in flight on `slot`; it reports its end as usual.
    fn cancel_recv(&mut self, d: &mut Self::Driver, slot: u32) -> Result<(), Refused>;
    /// Return a ring buffer named in [`Happened::RingData`].
    fn give_back(&mut self, d: &mut Self::Driver, buf: BufId);
    /// Send `from` on `slot`; it may end short. `lend` says whether the
    /// transport may send from the memory without copying it, where it
    /// can, and for how long it may then read it. One send of a slot is
    /// unreported at a time.
    fn send(
        &mut self,
        d: &mut Self::Driver,
        slot: u32,
        from: Span,
        lend: Lend,
    ) -> Result<(), Refused>;
    /// Shut the write side of `slot` down.
    fn shutdown_write(&mut self, d: &mut Self::Driver, slot: u32) -> Result<(), Refused>;
    /// Close `slot`; every operation in flight on it ends.
    fn close(&mut self, d: &mut Self::Driver, slot: u32) -> Result<(), Refused>;
    /// Close listener slot `listener`.
    fn close_listener(&mut self, d: &mut Self::Driver, listener: u32) -> Result<(), Refused>;
    /// The peer's address of the connection in `slot`, if known.
    fn peer_addr(&self, d: &Self::Driver, slot: u32) -> Option<SocketAddr>;

    /// What `completion` means, or `None` when it is not this transport's.
    fn translate(&mut self, d: &mut Self::Driver, completion: Self::Completion)
        -> Option<Happened>;
}
