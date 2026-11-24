// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a connection ends.
//!
//! An end is owed to the caller only when nothing received is left to
//! pull. A connection whose stream has ended with input left is
//! `Finishing`; pulling its last frame settles the end. With half-close a
//! peer that finished in order leaves the connection `HalfClosed`, open
//! for sending until the caller shuts its own side down.

use super::state::{St, EV_CLOSED, EV_DOWN, EV_FINISHED, EV_READABLE};
use super::Net;
use crate::event::Why;
use crate::transport::Transport;

impl<T: Transport> Net<T> {
    /// Whether what is queued on `slot` may still go out.
    pub(super) fn flushable(&self, slot: u32) -> bool {
        let c = &self.conns[slot as usize];
        match c.st {
            St::Up | St::Refused | St::HalfClosed => true,
            // The peer has finished, in order so far: with half-close an
            // answer goes out while the rest of the input is pulled.
            St::Finishing => c.opts.half_close && c.why == Why::Orderly,
            St::Free | St::Connecting | St::Securing | St::Dying | St::Closing | St::Backoff => {
                false
            }
        }
    }

    /// Set or clear the idle deadline of a connection that is up or
    /// half-closed.
    pub(super) fn arm_idle(&mut self, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.opts.idle_timeout_ns > 0 {
            let at = c.last_active.saturating_add(c.opts.idle_timeout_ns);
            self.deadlines.set(slot, at);
        } else {
            self.deadlines.clear(slot);
        }
    }

    pub(super) fn on_conn_deadline(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &self.conns[slot as usize];
        match c.st {
            St::Connecting => {
                // The close ends the connect in flight.
                self.conns[slot as usize].sock_open = true;
                self.end(slot, Why::Timeout);
            }
            St::Backoff => self.start_connect(d, slot),
            St::Up | St::HalfClosed => {
                let idle = c.opts.idle_timeout_ns;
                if idle > 0 && self.now >= c.last_active.saturating_add(idle) {
                    self.end(slot, Why::Timeout);
                } else {
                    self.arm_idle(slot);
                }
            }
            // The handshake of an accepted connection took too long.
            St::Securing => self.end(slot, Why::Timeout),
            St::Finishing => {
                let active_until = c.last_active.saturating_add(c.opts.idle_timeout_ns);
                if self.flushable(slot) && self.now < active_until {
                    // With half-close it is still answering: its input
                    // waits for as long as it sends.
                    return self.deadlines.set(slot, active_until);
                }
                // Its caller left its last input unpulled for too long.
                self.drop_input(slot);
                self.conns[slot as usize].why = Why::Timeout;
                self.settle_end(slot);
            }
            St::Free | St::Refused | St::Dying | St::Closing => {}
        }
    }

    /// The connection in `slot` has ended for a reason of the peer's or the
    /// network's. The caller is told once it has pulled what was received.
    pub(super) fn end(&mut self, slot: u32, why: Why) {
        if self.is_ending(slot) {
            return;
        }
        let c = &mut self.conns[slot as usize];
        match c.st {
            St::Refused | St::Securing => {
                // Never reported as a connection, so nothing to report now.
                c.why = why;
                self.gens.retire(slot);
                return self.begin_close(slot, false);
            }
            St::Finishing => {
                // A failure after the stream's end is how it ends.
                if c.why == Why::Orderly {
                    c.why = why;
                }
                return self.settle_end(slot);
            }
            St::Free
            | St::Connecting
            | St::Up
            | St::HalfClosed
            | St::Dying
            | St::Closing
            | St::Backoff => {}
        }
        c.why = why;
        self.deadlines.clear(slot);
        self.settle_end(slot);
        if self.conns[slot as usize].st == St::Finishing {
            self.queue_event(slot, EV_READABLE);
        }
    }

    /// The stream of `slot` has ended, for the reason it holds. While
    /// input is left to pull it is `Finishing`; after that the end is
    /// settled: half-closed when the peer finished in order and the
    /// caller may still send, otherwise owed to the caller.
    pub(super) fn settle_end(&mut self, slot: u32) {
        if self.has_input(slot) {
            let c = &mut self.conns[slot as usize];
            if c.st != St::Finishing {
                c.st = St::Finishing;
                // Counted from the end: how long its input may wait.
                let idle = c.opts.idle_timeout_ns;
                if idle > 0 {
                    self.deadlines.set(slot, self.now.saturating_add(idle));
                }
            }
            return;
        }
        let c = &mut self.conns[slot as usize];
        if c.why == Why::Orderly && c.mid_frame() {
            // The stream ended inside a frame.
            c.why = Why::Reset;
        }
        let ours_done = c.shut_sent && !c.shutting;
        if c.why == Why::Orderly && c.opts.half_close && !ours_done {
            // The caller is told, unless it has shut its side down
            // already and only its shutdown is still on its way.
            let tell = !c.shut;
            c.st = St::HalfClosed;
            c.last_active = self.now;
            self.arm_idle(slot);
            if tell {
                self.queue_event(slot, EV_FINISHED);
            }
            return;
        }
        c.st = St::Dying;
        self.deadlines.clear(slot);
        self.owe_end(slot);
    }

    /// Owe the caller the end of the `Dying` `slot`.
    fn owe_end(&mut self, slot: u32) {
        let c = &self.conns[slot as usize];
        let redial = c.opts.reconnect.is_some() && c.peer.is_some();
        self.queue_event(slot, if redial { EV_DOWN } else { EV_CLOSED });
    }

    /// The write side of `slot` is shut down.
    pub(super) fn on_shut_down(&mut self, slot: u32) {
        let c = &mut self.conns[slot as usize];
        c.shutting = false;
        if c.st == St::HalfClosed {
            // Both sides are done.
            self.settle_end(slot);
        }
        self.try_free(slot);
    }
}
