// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A connection's life: connecting, ending, closing, dialling again.

use super::state::{
    ConnState, St, EV_ACCEPTED, EV_CLOSED, EV_DOWN, EV_READABLE, EV_REFUSED, EV_UP, EV_WRITABLE,
    NONE, W_CLOSE, W_CONNECT, W_FLUSH, W_READ, W_REFUSE, W_SHUT,
};
use super::Net;
use crate::backoff::Backoff;
use crate::config::ConnOpts;
use crate::event::{Event, NetError, Why};
use crate::framing::Framing;
use crate::handle::Conn;
use crate::transport::Transport;
use std::net::SocketAddr;

impl<T: Transport> Net<T> {
    /// Whether `opts` fits the sizes this `Net` was built with.
    pub(super) fn fits(&self, opts: &ConnOpts) -> bool {
        match opts.framing {
            Framing::Bytes => true,
            Framing::Header(h) => {
                h.header_len() <= self.cfg.sizes.header_max as usize
                    && h.max_frame() <= self.cfg.sizes.frame_size as usize
            }
        }
    }

    /// Open a connection to `addr`. [`Event::Up`] follows when it is
    /// connected, or [`Event::Closed`] (with reconnecting, [`Event::Down`])
    /// when it could not be.
    pub fn connect(
        &mut self,
        d: &mut T::Driver,
        addr: SocketAddr,
        opts: ConnOpts,
    ) -> Result<Conn, NetError> {
        if !self.fits(&opts) {
            return Err(NetError::Framing);
        }
        let top = self.free_len.checked_sub(1).ok_or(NetError::NoSlots)?;
        self.free_len = top;
        let slot = self.free[top];
        let seed = self.cfg.sizes.seed ^ (u64::from(slot) << 32) ^ self.now;
        let c = &mut self.conns[slot as usize];
        *c = ConnState::free(opts, seed);
        c.peer = Some(addr);
        if let Some(r) = opts.reconnect {
            c.backoff = Backoff::new(r.base_ns, r.max_ns, r.jitter_per_mille, seed);
        }
        self.start_connect(d, slot);
        Ok(self.gens.conn(slot))
    }

    /// Dial the peer of `slot`, now or on the next `poll`.
    pub(super) fn start_connect(&mut self, d: &mut T::Driver, slot: u32) {
        let c = &mut self.conns[slot as usize];
        c.st = St::Connecting;
        c.closed = false;
        let (Some(addr), timeout) = (c.peer, c.opts.connect_timeout_ns) else {
            return;
        };
        if self.t.connect(d, slot, addr).is_ok() {
            self.conns[slot as usize].connecting = true;
        } else {
            self.add_work(slot, W_CONNECT);
        }
        if timeout > 0 {
            self.deadlines.set(slot, self.now.saturating_add(timeout));
        }
    }

    pub(super) fn on_connected(&mut self, d: &mut T::Driver, slot: u32, result: i32) {
        let c = &mut self.conns[slot as usize];
        c.connecting = false;
        if c.st != St::Connecting {
            // Timed out or closed meanwhile.
            c.sock_open |= result >= 0;
            return self.try_free(slot);
        }
        if result < 0 {
            // A connect that fails leaves no socket behind.
            return self.end(slot, Why::Reset);
        }
        c.sock_open = true;
        c.st = St::Up;
        c.last_active = self.now;
        c.backoff.reset();
        self.arm_idle(slot);
        self.queue_event(slot, EV_UP);
        self.start_reading(d, slot);
    }

    /// Set or clear the idle deadline of an `Up` connection.
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
            St::Up => {
                let idle = c.opts.idle_timeout_ns;
                if idle > 0 && self.now >= c.last_active.saturating_add(idle) {
                    self.end(slot, Why::Timeout);
                } else {
                    self.arm_idle(slot);
                }
            }
            St::Free | St::Refused | St::Dying | St::Closing => {}
        }
    }

    /// The connection in `slot` has ended for a reason of the peer's or the
    /// network's. The caller is told; the socket is closed once it has been.
    pub(super) fn end(&mut self, slot: u32, why: Why) {
        if self.is_ending(slot) {
            return;
        }
        let c = &mut self.conns[slot as usize];
        c.why = why;
        self.deadlines.clear(slot);
        if c.st == St::Refused {
            // Never reported as a connection, so nothing to report now.
            self.gens.retire(slot);
            self.begin_close(slot, false);
            return;
        }
        let redial = c.opts.reconnect.is_some() && c.peer.is_some();
        c.st = St::Dying;
        self.queue_event(slot, if redial { EV_DOWN } else { EV_CLOSED });
    }

    /// Start closing `slot`; the transport is told on the next `poll`.
    pub(super) fn begin_close(&mut self, slot: u32, redial: bool) {
        let c = &mut self.conns[slot as usize];
        c.st = St::Closing;
        c.redial = redial;
        c.ev = 0;
        self.deadlines.clear(slot);
        self.add_work(slot, W_CLOSE);
    }

    /// The event for bit `bit` of connection `slot`.
    pub(super) fn conn_event(&mut self, slot: u32, bit: u8) -> Event {
        let conn = self.gens.conn(slot);
        let c = &self.conns[slot as usize];
        let (why, l) = (c.why, c.listener);
        match bit {
            EV_UP => Event::Up(conn),
            EV_ACCEPTED => Event::Accepted {
                listener: self.lgens.listener(l),
                conn,
            },
            EV_REFUSED => {
                // From here on the connection closes once its queue is empty.
                self.add_work(slot, W_REFUSE);
                Event::Refused {
                    listener: self.lgens.listener(l),
                    conn,
                }
            }
            EV_READABLE => Event::Readable(conn),
            EV_WRITABLE => Event::Writable(conn),
            EV_DOWN => {
                self.begin_close(slot, true);
                Event::Down { conn, why }
            }
            _ => {
                self.gens.retire(slot);
                self.begin_close(slot, false);
                Event::Closed { conn, why }
            }
        }
    }

    /// Close a connection now. Its handle is stale at once and no event
    /// follows; what was queued to send is dropped.
    pub fn close(&mut self, d: &mut T::Driver, conn: Conn) -> Result<(), NetError> {
        let slot = self.gens.slot_of(conn).ok_or(NetError::Stale)?;
        self.gens.retire(slot);
        match self.conns[slot as usize].st {
            St::Free | St::Closing => {}
            St::Backoff => {
                self.deadlines.clear(slot);
                self.release_slot(slot);
            }
            St::Connecting | St::Up | St::Refused | St::Dying => {
                // A connect in flight is ended by the close.
                self.conns[slot as usize].sock_open = true;
                self.begin_close(slot, false);
                self.do_work(d, slot);
            }
        }
        Ok(())
    }

    /// What `poll` owes connection `slot`.
    pub(super) fn do_work(&mut self, d: &mut T::Driver, slot: u32) {
        let want = core::mem::take(&mut self.conns[slot as usize].want);
        if want & W_CONNECT != 0 && self.conns[slot as usize].st == St::Connecting {
            if self.t.connect(d, slot, self.peer_of(slot)).is_ok() {
                self.conns[slot as usize].connecting = true;
            } else {
                self.add_work(slot, W_CONNECT);
            }
        }
        if want & W_CLOSE != 0 {
            self.close_socket(d, slot);
        }
        if want & W_REFUSE != 0 {
            self.close_refused(slot);
        }
        if want & W_FLUSH != 0 {
            self.flush(d, slot);
        }
        if want & W_SHUT != 0 {
            self.shut_if_sent(d, slot);
        }
        if want & W_READ != 0 {
            self.resume_reading(d, slot);
        }
    }

    fn peer_of(&self, slot: u32) -> SocketAddr {
        let unspecified = SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0));
        self.conns[slot as usize].peer.unwrap_or(unspecified)
    }

    /// A refused connection closes once the one frame it may be sent has gone.
    pub(super) fn close_refused(&mut self, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.st == St::Refused && c.tx_len == 0 && !c.sending {
            self.gens.retire(slot);
            self.begin_close(slot, false);
        }
    }

    /// Hand the ring buffers back and close the socket.
    fn close_socket(&mut self, d: &mut T::Driver, slot: u32) {
        self.drop_chunks(slot);
        let c = &mut self.conns[slot as usize];
        if c.st != St::Closing {
            return;
        }
        if !c.sock_open {
            c.closed = true;
            return self.try_free(slot);
        }
        if self.t.close(d, slot).is_ok() {
            self.conns[slot as usize].sock_open = false;
        } else {
            self.add_work(slot, W_CLOSE);
        }
    }

    /// Free `slot` once its close and every operation in flight have ended.
    pub(super) fn try_free(&mut self, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.st != St::Closing || !c.closed || c.sock_open || c.in_flight() {
            return;
        }
        self.drop_rx(slot);
        self.drop_tx(slot);
        let ended = &mut self.conns[slot as usize];
        if ended.redial {
            ended.st = St::Backoff;
            ended.redial = false;
            let wait = ended.backoff.next_delay_ns();
            self.deadlines.set(slot, self.now.saturating_add(wait));
        } else {
            self.release_slot(slot);
        }
    }

    /// The slot holds nothing any more.
    fn release_slot(&mut self, slot: u32) {
        let c = &mut self.conns[slot as usize];
        let (listener, counted) = (c.listener, core::mem::take(&mut c.counted));
        c.st = St::Free;
        c.ev = 0;
        if listener != NONE && counted {
            let l = &mut self.listeners[listener as usize];
            l.count = l.count.saturating_sub(1);
        }
        self.conns[slot as usize].listener = NONE;
        if slot < self.cfg.sizes.connects {
            self.free[self.free_len] = slot;
            self.free_len += 1;
        }
    }

    /// The time the `Net` was last told, for tests and callers that need
    /// the same clock.
    pub const fn now_ns(&self) -> u64 {
        self.now
    }

    /// Whether `conn` is connected.
    pub fn is_up(&self, conn: Conn) -> bool {
        self.gens
            .slot_of(conn)
            .is_some_and(|slot| self.conns[slot as usize].st == St::Up)
    }

    /// The peer's address of `conn`, for a connection the caller opened or
    /// one accepted by a listener with peer addresses.
    pub fn peer(&self, d: &T::Driver, conn: Conn) -> Option<SocketAddr> {
        let slot = self.gens.slot_of(conn)?;
        let c = &self.conns[slot as usize];
        c.peer.or_else(|| self.t.peer_addr(d, slot))
    }

    /// Tell the caller a full queue has room again.
    pub(super) fn mark_writable(&mut self, slot: u32) {
        self.queue_event(slot, EV_WRITABLE);
    }
}
