// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A connection coming up and going down, and which ends a run can have.

use super::book::Sender;
use super::drive::Run;
use longshore_tcp::{Conn, Why};

/// Nothing in a run breaks the framing or a TLS record: the frames are
/// well formed, the certificate is the one expected, and the faults cut
/// streams short without changing a byte. An end for either reason is the
/// set's own doing, but for one: a handshake that the peer cut short ends
/// a connection that was never up as a TLS failure.
pub(crate) fn unbroken(why: Why, was_up: bool, tls: bool) -> Result<(), String> {
    match why {
        Why::Tls if tls && !was_up => Ok(()),
        Why::Protocol | Why::Tls => Err(format!("ended with {why:?}, which nothing here causes")),
        Why::Orderly | Why::Reset | Why::Timeout => Ok(()),
    }
}

impl Run {
    /// `conn` is up: once, until it goes down.
    pub(crate) fn came_up(&mut self, server: bool, conn: Conn) -> Result<(), String> {
        let at = self.named(server, conn)?;
        let h = &mut self.sides[usize::from(server)].handles[at];
        if core::mem::replace(&mut h.up, true) {
            return Err("up twice".to_string());
        }
        Ok(())
    }

    /// `conn` went down: only one that reconnects. It is told again for
    /// each attempt to dial that fails while it stays down.
    pub(crate) fn went_down(&mut self, server: bool, conn: Conn) -> Result<(), String> {
        let at = self.named(server, conn)?;
        let h = &mut self.sides[usize::from(server)].handles[at];
        if !h.reconnect {
            return Err("down without reconnecting".to_string());
        }
        self.stats.went_down += 1;
        if !core::mem::replace(&mut h.up, false) {
            return Ok(());
        }
        // What it had queued is dropped. Whatever it sends from here on,
        // which it can as soon as it is up, whether or not the caller has
        // pulled that event yet, is a new stream.
        let uid = h.uid;
        self.senders[uid as usize].aborted = true;
        self.restart(server, at);
        Ok(())
    }

    /// The handle at `at` went down: when it is up again its frames are
    /// numbered anew, as a new sender, and so are its peer's.
    fn restart(&mut self, server: bool, at: usize) {
        let uid = self.senders.len() as u32;
        let h = &mut self.sides[usize::from(server)].handles[at];
        (h.uid, h.peer, h.got, h.got_own) = (uid, None, 0, 0);
        (h.finished, h.full) = (false, false);
        let half_close = h.half_close;
        self.senders.push(Sender {
            server,
            sent: 0,
            own: 0,
            shut: false,
            aborted: false,
            half_close,
        });
    }
}
