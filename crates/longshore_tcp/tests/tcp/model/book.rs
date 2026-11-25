// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the model knows without looking inside `Net`: every handle it
//! was given and what became of it, and what each connection has sent.
//!
//! A frame says who sent it and which of that sender's frames it is, so
//! the receiver can tell loss, repetition and disorder from the frame
//! alone: a four-byte length, the sender's number, the frame's number,
//! then bytes that follow from the two.

use longshore_tcp::{Conn, Frame, Listener};

/// The fewest bytes of a frame after its length: the two numbers.
pub(crate) const BODY_MIN: usize = 8;

/// What one end has sent, by the number its frames carry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sender {
    /// On the server set.
    pub(crate) server: bool,
    /// Frames queued in all, relayed ones included, and of its own.
    pub(crate) sent: u32,
    pub(crate) own: u32,
    /// Its write side was shut down in order, after which nothing is sent.
    pub(crate) shut: bool,
    /// It was closed outright: what it had queued may not have gone.
    pub(crate) aborted: bool,
    /// Its connection has half-close: its peer's end does not drop what
    /// it has queued.
    pub(crate) half_close: bool,
}

/// A stream that ended in order, as its receiver saw it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ended {
    /// The sender at the other end.
    pub(crate) peer: u32,
    /// The receiver's own sender, whose finishing first may have made the
    /// peer drop what it had queued.
    pub(crate) receiver: u32,
    pub(crate) got: u32,
    pub(crate) got_own: u32,
}

/// A connection handle the caller was given.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Handle {
    pub(crate) conn: Conn,
    /// The number its own frames carry.
    pub(crate) uid: u32,
    /// Not ended, as far as the caller has been told.
    pub(crate) alive: bool,
    /// Accepted over a limit: it may end without a word.
    pub(crate) refused: bool,
    pub(crate) half_close: bool,
    /// It dials again when it goes down, under the same handle.
    pub(crate) reconnect: bool,
    pub(crate) up: bool,
    pub(crate) finished: bool,
    /// A send was refused for want of room: it is owed word of room.
    pub(crate) full: bool,
    /// The sender at the other end, once a frame of its own has come.
    pub(crate) peer: Option<u32>,
    /// Frames received in all, and of the peer's own.
    pub(crate) got: u32,
    pub(crate) got_own: u32,
}

impl Handle {
    /// A handle just given to the caller; its number is set when adopted.
    pub(crate) const fn new(conn: Conn, half_close: bool, up: bool, refused: bool) -> Handle {
        Handle {
            conn,
            uid: 0,
            alive: true,
            refused,
            half_close,
            reconnect: false,
            up,
            finished: false,
            full: false,
            peer: None,
            got: 0,
            got_own: 0,
        }
    }
}

/// What one set's caller holds.
#[derive(Default)]
pub(crate) struct Side {
    pub(crate) handles: Vec<Handle>,
    /// Listeners, whether still open, and whether their connections are
    /// half-close.
    pub(crate) listeners: Vec<(Listener, bool, bool)>,
    pub(crate) held: Vec<Frame>,
}

impl Side {
    /// The handle `conn`, if the caller was ever given it.
    pub(crate) fn find(&self, conn: Conn) -> Option<usize> {
        self.handles.iter().rposition(|h| h.conn == conn)
    }
}

/// Byte `at` of the body of frame `seq` of sender `uid`, after its numbers.
const fn pattern(uid: u32, seq: u32, at: usize) -> u8 {
    (at as u8)
        .wrapping_mul(31)
        .wrapping_add(uid as u8)
        .wrapping_add((seq as u8).wrapping_mul(7))
}

/// Write frame `seq` of sender `uid` into `to`, length prefix included.
pub(crate) fn fill(to: &mut [u8], uid: u32, seq: u32) {
    let body = to.len() - 4;
    to[..4].copy_from_slice(&(body as u32).to_be_bytes());
    to[4..8].copy_from_slice(&uid.to_le_bytes());
    to[8..12].copy_from_slice(&seq.to_le_bytes());
    for (at, b) in to[12..].iter_mut().enumerate() {
        *b = pattern(uid, seq, at);
    }
}

/// The sender and number of `frame`, if it is a frame as `fill` wrote it.
pub(crate) fn read(frame: &[u8]) -> Result<(u32, u32), String> {
    if frame.len() < 4 + BODY_MIN {
        return Err(format!("a frame of {} bytes", frame.len()));
    }
    let said = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if said != frame.len() - 4 {
        return Err(format!("a frame of {} bytes says {said}", frame.len()));
    }
    let uid = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
    let seq = u32::from_le_bytes([frame[8], frame[9], frame[10], frame[11]]);
    let whole = frame[12..]
        .iter()
        .enumerate()
        .all(|(at, b)| *b == pattern(uid, seq, at));
    if whole {
        Ok((uid, seq))
    } else {
        Err(format!("frame {seq} of sender {uid} has other bytes"))
    }
}
