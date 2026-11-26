// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a transport reports ([`Happened`]), and how long a send may read
//! the memory it is given ([`Lend`]).

use longshore_common::io::BufId;

/// How long [`super::Transport::send`] may go on reading the memory it sends
/// from, in place of copying it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lend {
    /// Not at all: the memory is free when the send is reported.
    No,
    /// Until the send is reported: [`Happened::Sent`] then comes only when
    /// the memory is free again, which over a network is when the peer
    /// has acknowledged the bytes.
    UntilSent,
    /// Past the report of the send, so that the next send can follow as
    /// soon as the bytes were taken. The send is reported with
    /// [`Happened::Taken`] while the transport still reads the memory, and
    /// [`Happened::Released`] with this number follows when it no longer
    /// does; or with [`Happened::Sent`], when the memory is free already
    /// and nothing follows. The number is the caller's: below the
    /// `Sizes::with_zero_copy_sends` of its `Net`, and not that of
    /// another send of the slot that has not been released.
    UntilReleased(u32),
}

/// What a transport reports. Results are byte counts or 0 on success, and
/// negated Linux error numbers on failure, as everywhere in longshore.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Happened {
    /// The completion was the transport's own business; nothing for `Net`.
    Nothing,
    /// A connect ended.
    Connected {
        /// The connection slot.
        slot: u32,
        /// 0, or a negated error.
        result: i32,
    },
    /// A TLS handshake started with [`super::Transport::secure`] ended: 0, or a
    /// negated error ([`super::TLS_FAILED`], or `-ECANCELED` when the connection
    /// was closed under it).
    Secured {
        /// The connection slot.
        slot: u32,
        /// 0, or a negated error.
        result: i32,
    },
    /// A listener accepted a connection into `slot`. `armed` says whether
    /// the listener's armed accept is still running (always false for a
    /// single accept).
    Accepted {
        /// The listener slot that accepted.
        listener: u32,
        /// The connection slot the connection took.
        slot: u32,
        /// Whether the listener's armed accept is still running.
        armed: bool,
    },
    /// An accept ended without a connection. `slot` is the slot a single
    /// accept was to fill.
    AcceptFailed {
        /// The listener slot.
        listener: u32,
        /// The slot a single accept was to fill.
        slot: Option<u32>,
        /// The negated error.
        result: i32,
    },
    /// An armed receive delivered `len` bytes in ring buffer `buf`, which
    /// `Net` gives back when done with it. `armed` says whether more follow.
    RingData {
        /// The connection slot.
        slot: u32,
        /// The ring buffer holding the bytes.
        buf: BufId,
        /// How many bytes.
        len: u32,
        /// Whether the armed receive is still running.
        armed: bool,
    },
    /// A zero-copy receive ([`super::Transport::recv_zc`]) delivered a
    /// piece: `len` bytes of page `page` of the receive area, from `off`.
    /// `Net` returns it with [`super::Transport::give_back_piece`] when it
    /// is done with it, as it was delivered. The receive's end is
    /// reported with [`Happened::RecvEnded`].
    Piece {
        /// The connection slot.
        slot: u32,
        /// The page of the receive area the bytes lie in.
        page: BufId,
        /// Where the bytes start in the page.
        off: u32,
        /// How many bytes.
        len: u32,
    },
    /// An armed receive ended without bytes: 0 at end of stream, a negated
    /// error otherwise (`-ENOBUFS` when the ring is empty, `-ECANCELED`
    /// when it was cancelled). On a protected slot the end of stream is
    /// the bytes' own: whether the peer ended in order is what
    /// [`super::Transport::open`] says of the records before it.
    RecvEnded {
        /// The connection slot.
        slot: u32,
        /// 0 at end of stream, or a negated error.
        result: i32,
    },
    /// A receive into frame memory ended.
    Received {
        /// The connection slot.
        slot: u32,
        /// Bytes received, 0 at end of stream, or a negated error.
        result: i32,
    },
    /// A send ended; the transport no longer uses its buffer.
    Sent {
        /// The connection slot.
        slot: u32,
        /// Bytes sent, or a negated error.
        result: i32,
    },
    /// A send lent its memory with [`Lend::UntilReleased`] has taken its
    /// bytes, and the transport still reads the memory: the next send may
    /// follow, and [`Happened::Released`] comes for this one.
    Taken {
        /// The connection slot.
        slot: u32,
        /// Bytes taken; more than none.
        result: i32,
    },
    /// The transport no longer reads the memory of a send it reported
    /// with [`Happened::Taken`]. Once for each, in no fixed order among
    /// the sends of a slot, and for a slot that is closed before its
    /// [`Happened::Closed`] or after it.
    Released {
        /// The connection slot.
        slot: u32,
        /// The number the send was given.
        id: u32,
    },
    /// A shutdown of the write side ended.
    ShutDown {
        /// The connection slot.
        slot: u32,
        /// 0, or a negated error.
        result: i32,
    },
    /// A connection slot is closed and free.
    Closed {
        /// The connection slot.
        slot: u32,
    },
    /// A listener slot is closed and free.
    ListenerClosed {
        /// The listener slot.
        listener: u32,
    },
}
