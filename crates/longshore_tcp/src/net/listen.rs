// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Listening: binding (with retry), accepting (armed, or one at a time
//! with peer addresses), the connection limit, and backing off after an
//! accept fails.

use super::cover::Col;
use super::state::{LSt, ListenerState, St, EV_ACCEPTED, EV_REFUSED, NONE};
use super::Net;
use crate::addr::Addr;
use crate::config::ListenConfig;
use crate::event::{Event, NetError};
use crate::handle::Listener;
use crate::transport::{Listening, Transport};
use longshore_common::io::{EADDRINUSE, ECANCELED};
use std::net::SocketAddr;

/// Between two tries to bind an address that is in use.
const BIND_RETRY_NS: u64 = 50_000_000;
/// The first wait after an accept fails, doubling up to the longest.
const ACCEPT_BACKOFF_MIN_NS: u64 = 5_000_000;
const ACCEPT_BACKOFF_MAX_NS: u64 = 1_000_000_000;

impl<T: Transport> Net<T> {
    /// Listen on `addr`. With a bind retry, an address in use is tried
    /// again until the time is up, and [`Event::ListenFailed`] says if it
    /// never worked; without one the error is returned at once.
    pub fn listen(
        &mut self,
        d: &mut T::Driver,
        addr: impl Into<Addr>,
        cfg: ListenConfig,
    ) -> Result<Listener, NetError> {
        let addr: Addr = addr.into();
        self.now = self.t.now_ns(d);
        if !self.fits(&cfg.conn) {
            return Err(NetError::Framing);
        }
        if cfg.tls {
            self.fits_tls(&cfg.conn, false)?;
        }
        // A slot whose failed bind is still to be reported keeps its handle.
        let unused = |l: &ListenerState| l.st == LSt::Free && l.failed.is_none();
        let free = self.listeners.iter().position(unused);
        let l = free.ok_or(NetError::NoSlots)? as u32;
        let state = &mut self.listeners[l as usize];
        state.cfg = cfg;
        state.addr = addr;
        (state.bound, state.count, state.armed, state.single) = (None, 0, false, None);
        (state.backoff_ns, state.waiting) = (0, false);
        (state.close_sent, state.close_done) = (false, false);
        match self.bind(d, l) {
            Ok(()) => {}
            Err(e) if e == -EADDRINUSE && cfg.bind_retry_ns > 0 => {
                let waiting = &mut self.listeners[l as usize];
                waiting.st = LSt::Binding;
                waiting.bind_until = self.now.saturating_add(cfg.bind_retry_ns);
                let slot = self.cfg.conns() + l;
                self.deadlines
                    .set(slot, self.now.saturating_add(BIND_RETRY_NS));
            }
            Err(e) => return Err(NetError::Listen(e)),
        }
        Ok(self.lgens.listener(l))
    }

    /// Bind and listen; the negated error number when it fails.
    fn bind(&mut self, d: &mut T::Driver, l: u32) -> Result<(), i32> {
        let state = &self.listeners[l as usize];
        let how = Listening::new(state.cfg.reuse_port, state.cfg.backlog);
        match self.t.listen(d, l, &state.addr, how) {
            Ok(bound) => {
                let up = &mut self.listeners[l as usize];
                up.st = LSt::Listening;
                up.bound = bound;
                self.start_accept(d, l);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Err(-EADDRINUSE),
            Err(e) => Err(-e.raw_os_error().unwrap_or(22)),
        }
    }

    /// The address a listener is bound to, once it is.
    pub fn listener_addr(&self, listener: Listener) -> Option<SocketAddr> {
        let l = self.lgens.slot_of_listener(listener)?;
        self.listeners[l as usize].bound
    }

    /// Stop listening. Connections it accepted stay.
    pub fn close_listener(
        &mut self,
        d: &mut T::Driver,
        listener: Listener,
    ) -> Result<(), NetError> {
        let l = self
            .lgens
            .slot_of_listener(listener)
            .ok_or(NetError::Stale)?;
        self.lgens.retire(l);
        self.stop_listener(d, l);
        Ok(())
    }

    pub(super) fn stop_listener(&mut self, d: &mut T::Driver, l: u32) {
        let state = &mut self.listeners[l as usize];
        self.deadlines.clear(self.cfg.conns() + l);
        // Its handle is stale: there is nobody to report a failed bind to.
        state.failed = None;
        match state.st {
            LSt::Free | LSt::Closing => {}
            LSt::Binding => state.st = LSt::Free,
            LSt::Listening => {
                state.st = LSt::Closing;
                self.send_listener_close(d, l);
            }
        }
    }

    /// Hand the close of listener `l` to the transport, unless it has it
    /// already. `poll` tries again when the driver has no room.
    fn send_listener_close(&mut self, d: &mut T::Driver, l: u32) {
        if !self.listeners[l as usize].close_sent && self.t.close_listener(d, l).is_ok() {
            self.listeners[l as usize].close_sent = true;
        }
    }

    pub(super) fn on_listener_closed(&mut self, l: u32) {
        self.listeners[l as usize].close_done = true;
        self.free_listener(l);
    }

    /// Free the closing listener `l` once its close has completed and its
    /// accept has made its last report: until then a report for the slot
    /// is this listener's, and must not find another one there.
    fn free_listener(&mut self, l: u32) {
        let state = &mut self.listeners[l as usize];
        let quiet = state.close_done && !state.armed && state.single.is_none();
        if state.st == LSt::Closing && quiet {
            state.st = LSt::Free;
            (state.close_sent, state.close_done) = (false, false);
        }
    }

    /// Keep an accept running on listener `l`, if it should have one.
    pub(super) fn start_accept(&mut self, d: &mut T::Driver, l: u32) {
        let state = &self.listeners[l as usize];
        if state.st == LSt::Closing {
            return self.send_listener_close(d, l);
        }
        if state.st != LSt::Listening || state.waiting || self.draining.is_some() {
            return;
        }
        if !state.cfg.peer_addr {
            if !state.armed && self.t.accept_armed(d, l).is_ok() {
                self.listeners[l as usize].armed = true;
            }
            return;
        }
        if state.single.is_some() {
            return;
        }
        // One accept per connection, into a slot of our choosing.
        let Some(top) = self.free_len.checked_sub(1) else {
            return;
        };
        let slot = self.free[top];
        if self.t.accept_into(d, l, slot).is_ok() {
            self.free_len = top;
            self.listeners[l as usize].single = Some(slot);
        }
    }

    pub(super) fn on_accepted(&mut self, d: &mut T::Driver, l: u32, slot: u32, armed: bool) {
        self.cover(slot, Col::Accepted);
        let state = &mut self.listeners[l as usize];
        if state.cfg.peer_addr {
            state.single = None;
        } else {
            state.armed = armed;
        }
        state.backoff_ns = 0;
        // A listener that is closing has had its handle retired: the
        // handle of the slot is already the next listener's, which must
        // not be given this connection.
        let by = (state.st == LSt::Listening).then(|| self.lgens.listener(l));
        let c = &mut self.conns[slot as usize];
        if c.st == St::Free {
            self.occupy(d, slot, l, true);
        } else {
            // Every completion of the slot is its last connection's until
            // that one is freed: nothing is started for the new one before.
            (c.parked, c.parked_by) = (l, by);
        }
        self.free_listener(l);
        self.start_accept(d, l);
    }

    /// The connection the kernel put in `slot` while the one before it
    /// was still ending takes the slot, now that it is free.
    pub(super) fn admit_parked(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &mut self.conns[slot as usize];
        let l = core::mem::replace(&mut c.parked, NONE);
        let by = c.parked_by.take();
        if l != NONE && c.st == St::Free {
            // Its listener may have gone, and another taken its slot; or
            // it was closing already when the connection came.
            let live = by.is_some_and(|h| self.lgens.slot_of_listener(h) == Some(l));
            self.occupy(d, slot, l, live);
        }
    }

    /// Make `slot`, free until now, the connection listener `l` accepted.
    fn occupy(&mut self, d: &mut T::Driver, slot: u32, l: u32, live: bool) {
        let state = &self.listeners[l as usize];
        let (cfg, listening) = (state.cfg, live && state.st == LSt::Listening);
        let by = self.lgens.listener(l);
        let unix = matches!(state.addr, Addr::Unix(_));
        let seed = self.cfg.sizes.seed ^ u64::from(slot);
        let c = &mut self.conns[slot as usize];
        c.renew(cfg.conn, seed);
        c.accepted_by = if live { by } else { Listener::NONE };
        c.unix = unix;
        c.sock_open = true;
        c.last_active = self.now;
        if !listening {
            // Accepted while the listener was closing: not a connection.
            c.st = St::Refused;
            self.gens.retire(slot);
            self.begin_close(slot, false);
            return;
        }
        if cfg.tls {
            // Announced, and counted, once its handshake is done.
            (c.st, c.secure) = (St::Securing, true);
            if cfg.conn.handshake_timeout_ns > 0 {
                let at = self.now.saturating_add(cfg.conn.handshake_timeout_ns);
                self.deadlines.set(slot, at);
            }
            self.begin_secure(d, slot, false);
        } else {
            self.admit(d, slot);
        }
    }

    /// Announce the accepted connection in `slot`: as a connection, or at
    /// the listener's limit as one to refuse.
    pub(super) fn admit(&mut self, d: &mut T::Driver, slot: u32) {
        // The listener that accepted it, not a later one in its slot.
        let by = self.conns[slot as usize].accepted_by;
        let listening = self
            .lgens
            .slot_of_listener(by)
            .filter(|l| self.listeners[*l as usize].st == LSt::Listening);
        self.deadlines.clear(slot);
        let c = &mut self.conns[slot as usize];
        let Some(l) = listening else {
            // The listener closed during the handshake: not a connection.
            c.st = St::Refused;
            self.gens.retire(slot);
            return self.begin_close(slot, false);
        };
        let state = &self.listeners[l as usize];
        if state.count >= state.cfg.limit {
            c.st = St::Refused;
            self.queue_event(slot, EV_REFUSED);
        } else {
            c.st = St::Up;
            c.counted = true;
            c.last_active = self.now;
            self.listeners[l as usize].count += 1;
            self.arm_idle(slot);
            self.queue_event(slot, EV_ACCEPTED);
            self.start_reading(d, slot);
        }
    }
    pub(super) fn on_accept_failed(&mut self, l: u32, slot: Option<u32>, result: i32) {
        let state = &mut self.listeners[l as usize];
        match slot {
            Some(slot) => {
                state.single = None;
                self.free[self.free_len] = slot;
                self.free_len += 1;
            }
            None => state.armed = false,
        }
        if state.st == LSt::Closing {
            return self.free_listener(l);
        }
        if state.st != LSt::Listening || result == -ECANCELED {
            return;
        }
        // Out of descriptors, most likely: wait before accepting again.
        let wait = (state.backoff_ns * 2).clamp(ACCEPT_BACKOFF_MIN_NS, ACCEPT_BACKOFF_MAX_NS);
        (state.backoff_ns, state.waiting) = (wait, true);
        let at = self.now.saturating_add(wait);
        self.deadlines.set(self.cfg.conns() + l, at);
    }

    pub(super) fn on_listener_deadline(&mut self, d: &mut T::Driver, l: u32) {
        let state = &mut self.listeners[l as usize];
        match state.st {
            LSt::Listening => {
                // The wait after a failed accept is over. Its length stays
                // until an accept works, so that the next failure waits
                // longer; `poll` tries again if the driver has no room.
                state.waiting = false;
                self.start_accept(d, l);
            }
            LSt::Binding => match self.bind(d, l) {
                Ok(()) => {}
                Err(e) if e == -EADDRINUSE && self.now < self.listeners[l as usize].bind_until => {
                    let at = self.now.saturating_add(BIND_RETRY_NS);
                    self.deadlines.set(self.cfg.conns() + l, at);
                }
                Err(e) => {
                    let gone = &mut self.listeners[l as usize];
                    gone.st = LSt::Free;
                    gone.failed = Some(e);
                }
            },
            LSt::Free | LSt::Closing => {}
        }
    }

    /// A failed bind to report, if any.
    pub(super) fn listener_event(&mut self) -> Option<Event> {
        for l in 0..self.listeners.len() {
            if let Some(errno) = self.listeners[l].failed.take() {
                let listener = self.lgens.listener(l as u32);
                self.lgens.retire(l as u32);
                return Some(Event::ListenFailed { listener, errno });
            }
        }
        None
    }

    /// Stop accepting and report [`Event::Drained`] once every queued send
    /// has gone, or at `until_ns` at the latest. Connections stay open.
    pub fn drain(&mut self, d: &mut T::Driver, until_ns: u64) {
        self.draining = Some(until_ns);
        self.drain_reported = false;
        for l in 0..self.cfg.sizes.listeners {
            self.lgens.retire(l);
            self.stop_listener(d, l);
        }
    }
}
