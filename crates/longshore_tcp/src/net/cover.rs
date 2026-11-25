// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Which cells of the connection state table a `Net` has been in: how
//! often each kind of event reached a connection in each state. Counted
//! in debug builds only, for the model test's report of what it reached.

use super::state::St;
use super::Net;
use crate::handle::Conn;
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

/// How often each event reached a connection in each state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Coverage {
    hits: [[u32; Coverage::EVENTS.len()]; Coverage::STATES.len()],
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
    pub const EVENTS: [&'static str; 15] = [
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
    ];

    /// Nothing counted.
    pub const NONE: Coverage = Coverage {
        hits: [[0; Coverage::EVENTS.len()]; Coverage::STATES.len()],
    };

    /// How often event `event` reached a connection in state `state`, by
    /// their places in [`Coverage::STATES`] and [`Coverage::EVENTS`].
    pub const fn hits(&self, state: usize, event: usize) -> u32 {
        self.hits[state][event]
    }

    /// Add what `other` counted.
    pub fn add(&mut self, other: &Coverage) {
        for (row, more) in self.hits.iter_mut().zip(other.hits.iter()) {
            for (cell, n) in row.iter_mut().zip(more.iter()) {
                *cell = cell.saturating_add(*n);
            }
        }
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
        if cfg!(debug_assertions) {
            let st = self.conns[slot as usize].st;
            let cell = &mut self.coverage.hits[row(st)][col as usize];
            *cell = cell.saturating_add(1);
        }
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
