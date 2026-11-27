// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Which cells of the connection state table a `Net` has been in: how
//! often each kind of event reached a connection in each state. The same
//! for its listeners and the listener state table. Counted
//! in debug builds only, for the model test's report of what it reached.

use super::state::{LSt, St};
use super::Net;
use crate::handle::Conn;
use crate::hits::Hits;
use crate::transport::Transport;

/// What can reach a connection: the columns of the state table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Col {
    Close,
    Shutdown,
    Send,
    NextFrame,
    Connected,
    Secured,
    Data,
    EndOfStream,
    ReceiveEndedOtherwise,
    Sent,
    ShutDown,
    Closed,
    Deadline,
    EventPulled,
    Accepted,
    Released,
}

impl Col {
    /// The column of a receive that ended with `result`.
    pub(super) const fn of_receive(result: i32) -> Col {
        if result > 0 {
            Col::Data
        } else if result == 0 {
            Col::EndOfStream
        } else {
            Col::ReceiveEndedOtherwise
        }
    }
}

/// What can reach a listener: the columns of the listener state table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LCol {
    Listen,
    Close,
    Accepted,
    AcceptFailed,
    Closed,
    Deadline,
}

/// How often each event reached a connection, and a listener, in each
/// state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Coverage {
    hits: Hits<{ Coverage::STATES.len() }, { Coverage::EVENTS.len() }>,
    listeners: Hits<{ Coverage::LISTENER_STATES.len() }, { Coverage::LISTENER_EVENTS.len() }>,
}

impl Coverage {
    /// The states, in the order of the table's rows.
    pub const STATES: [&'static str; 10] = [
        "Free",
        "Connecting",
        "Securing",
        "Up",
        "Refused",
        "Finishing",
        "HalfClosed",
        "Dying",
        "Closing",
        "Backoff",
    ];

    /// The events, in the order of the table's columns.
    pub const EVENTS: [&'static str; 16] = [
        "close()",
        "shutdown_write()",
        "a send",
        "next_frame()",
        "Connected",
        "Secured",
        "data",
        "end of stream",
        "receive ended otherwise",
        "Sent",
        "ShutDown",
        "Closed",
        "deadline",
        "event pulled",
        "accepted into the slot",
        "Released",
    ];

    /// The listener states, in the order of their table's rows.
    pub const LISTENER_STATES: [&'static str; 4] = ["Free", "Binding", "Listening", "Closing"];

    /// The events of a listener, in the order of their table's columns.
    pub const LISTENER_EVENTS: [&'static str; 6] = [
        "listen()",
        "close_listener(), drain()",
        "accept ok",
        "accept failed",
        "Closed",
        "deadline",
    ];

    /// Nothing counted.
    pub const NONE: Coverage = Coverage {
        hits: Hits::NONE,
        listeners: Hits::NONE,
    };

    /// How often event `event` reached a listener in state `state`, by
    /// their places in [`Coverage::LISTENER_STATES`] and
    /// [`Coverage::LISTENER_EVENTS`].
    pub const fn listener_hits(&self, state: usize, event: usize) -> u32 {
        self.listeners.get(state, event)
    }

    /// How often event `event` reached a connection in state `state`, by
    /// their places in [`Coverage::STATES`] and [`Coverage::EVENTS`].
    pub const fn hits(&self, state: usize, event: usize) -> u32 {
        self.hits.get(state, event)
    }

    /// Add what `other` counted.
    pub fn add(&mut self, other: &Coverage) {
        self.hits.add(&other.hits);
        self.listeners.add(&other.listeners);
    }
}

impl Default for Coverage {
    fn default() -> Coverage {
        Coverage::NONE
    }
}

const fn row(st: St) -> usize {
    match st {
        St::Free => 0,
        St::Connecting => 1,
        St::Securing => 2,
        St::Up => 3,
        St::Refused => 4,
        St::Finishing => 5,
        St::HalfClosed => 6,
        St::Dying => 7,
        St::Closing => 8,
        St::Backoff => 9,
    }
}

impl<T: Transport> Net<T> {
    /// Count `col` reaching connection `slot` in the state it is in.
    #[inline]
    pub(super) fn cover(&mut self, slot: u32, col: Col) {
        let st = self.conns[slot as usize].st;
        self.coverage.hits.count(row(st), col as usize);
    }

    /// Count `col` reaching listener `l` in the state it is in.
    #[inline]
    pub(super) fn cover_listener(&mut self, l: u32, col: LCol) {
        let row = match self.listeners[l as usize].st {
            LSt::Free => 0,
            LSt::Binding => 1,
            LSt::Listening => 2,
            LSt::Closing => 3,
        };
        self.coverage.listeners.count(row, col as usize);
    }

    /// Count `col` reaching the connection `conn` names, if it names one.
    #[inline]
    pub(super) fn cover_call(&mut self, conn: Conn, col: Col) {
        if cfg!(debug_assertions) {
            if let Some(slot) = self.gens.slot_of(conn) {
                self.cover(slot, col);
            }
        }
    }

    /// What has been counted so far; nothing in a release build.
    pub const fn coverage(&self) -> Coverage {
        self.coverage
    }
}
