// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a `Net` tells its caller, and why it refuses.

use crate::handle::{Conn, Listener};

/// How a connection ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// The peer finished: everything it sent was delivered first.
    Orderly,
    /// The connection broke: a reset, a failed connect, or an end of
    /// stream in the middle of a frame.
    Reset,
    /// A connect or idle timeout passed.
    Timeout,
    /// The peer announced a frame that the framing does not allow.
    Protocol,
    /// TLS failed: the handshake did not complete, a record did not
    /// authenticate, or the peer sent an alert.
    Tls,
}

impl Why {
    /// Why a connection ended that failed with the negated error `result`.
    pub(crate) const fn of(result: i32) -> Why {
        if result == crate::transport::TLS_FAILED {
            Why::Tls
        } else {
            Why::Reset
        }
    }
}

/// Something for the caller to act on; pulled with `Net::next_event`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// A connection the caller opened is up, or up again.
    Up(Conn),
    /// A listener accepted a connection.
    Accepted {
        /// The listener that accepted.
        listener: Listener,
        /// The connection accepted.
        conn: Conn,
    },
    /// A listener at its limit accepted a connection only to refuse it:
    /// the caller may send it one frame, and it is closed after that.
    Refused {
        /// The listener at its limit.
        listener: Listener,
        /// The connection refused.
        conn: Conn,
    },
    /// Frames are waiting: pull them with `Net::next_frame` until it
    /// returns `None`.
    Readable(Conn),
    /// A send queue that was full has room again.
    Writable(Conn),
    /// With half-close: the peer has finished its stream in order and
    /// everything it sent has been pulled. The connection can still be
    /// sent on; it ends after `Net::shutdown_write`, or with `Net::close`.
    Finished(Conn),
    /// A reconnecting connection went down; its handle stays, and
    /// [`Event::Up`] follows when it is back. What was queued to send is
    /// dropped.
    Down {
        /// The connection that went down.
        conn: Conn,
        /// Why it went down.
        why: Why,
    },
    /// A connection ended. Its handle is stale from here on.
    Closed {
        /// The connection that ended.
        conn: Conn,
        /// How it ended.
        why: Why,
    },
    /// A listener that was retrying its bind gave up; `errno` is negated.
    ListenFailed {
        /// The listener that gave up.
        listener: Listener,
        /// The negated error.
        errno: i32,
    },
    /// Draining is over: queued sends have gone, or the deadline passed.
    Drained,
}

/// Why a send was not queued.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendError {
    /// The handle no longer names a connection.
    Stale,
    /// The connection is not up.
    NotUp,
    /// The connection's queue or the segment pool is full. `Event::Writable`
    /// follows when there is room.
    Full,
    /// Longer than a send segment.
    TooLarge,
    /// The frame is lent from the buffer ring: hold it first.
    NotHeld,
}

/// Why `Net` refused something other than a send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetError {
    /// The handle no longer names a connection or listener.
    Stale,
    /// Every slot of the kind needed is in use.
    NoSlots,
    /// The framing's header or longest frame does not fit the sizes the
    /// `Net` was built with. Also TLS in ring mode on frame memory that
    /// cannot serve it: buffers smaller than a record, or fewer than two
    /// of them (`Sizes::with_frames`).
    Framing,
    /// Listening failed with this negated error number.
    Listen(i32),
    /// No frame memory is free.
    NoMemory,
    /// The connection is not open for what was asked: not up yet, ended,
    /// or refused.
    NotUp,
    /// TLS was asked for and cannot be had: the `Net` was sized without
    /// `Sizes::with_tls`, the transport has no setup for this side, or the
    /// server name is empty or too long.
    NoTls,
}
