// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A fixed set of connections and listeners, driven from the caller's loop.
//!
//! `Net` borrows its transport's driver on every call and never waits. One
//! turn of the owner's loop:
//!
//! 1. tick the driver once, and hand each completion whose token is in the
//!    `Net`'s range to [`Net::on_completion`];
//! 2. pull events with [`Net::next_event`]; on [`Event::Readable`] pull
//!    frames with [`Net::next_frame`] until it returns `None`, and queue
//!    replies with `send_frame` or `send_lent`;
//! 3. call [`Net::poll`] with the current time: queued sends go to the
//!    driver, deadlines fire, ring buffers return;
//! 4. sleep no longer than [`Net::next_deadline`] allows.
//!
//! Completions only mark connections, and events and frames are pulled, so
//! nothing can overflow and not pulling is itself backpressure.

use crate::addr::ServerName;
use crate::config::{Config, ConnOpts, Rx};
use crate::deadline::Deadlines;
use crate::event::Event;
use crate::framing::Framing;
use crate::handle::Generations;
use crate::transport::{Happened, Transport};
use longshore_common::io::extents::Extents;
use longshore_common::io::{BufId, Limits};
use state::{Chunk, ConnState, Keep, ListenerState, Socket, St, TxSpan, NONE};

mod conn;
mod cover;
mod ending;
mod frames;
mod listen;
mod paged;
mod pins;
mod pull;
mod runs;
mod rx;
mod secure;
mod spill;
mod state;
mod tx;
mod usage;
mod zc;

pub use cover::Coverage;
pub use frames::Frame;
pub use paged::Spans;
pub use runs::{Pages, Run};
pub use usage::{Paged, Usage};

/// Connections and listeners over a transport `T`.
pub struct Net<T: Transport> {
    t: T,
    cfg: Config,
    gens: Generations,
    lgens: Generations,
    conns: Box<[ConnState]>,
    /// `Sizes::spans` queued sends per connection, as rings.
    spans: Box<[TxSpan]>,
    listeners: Box<[ListenerState]>,
    extents: Extents,
    /// Frames in pages (`Rx::ZeroCopy`): their runs, tail blocks, budget.
    zc: Pages,
    /// Free send segments, as a stack of indexes.
    segs: Box<[u32]>,
    segs_len: usize,
    /// Per connection, a place for each zero-copy send whose memory the
    /// transport may still read: what the send keeps.
    pins: Box<[Option<Keep>]>,
    /// How many of them are out, in all.
    pins_out: u32,
    /// Per send segment: the zero-copy sends that still read it, and
    /// whether its span is gone, so that it returns to the pool with the
    /// last of them.
    seg_pins: Box<[u32]>,
    seg_parked: Box<[bool]>,
    /// Connections refused a send segment because none was free, as a
    /// ring: one is told each time a segment returns.
    seg_waiters: Box<[u32]>,
    seg_wait_head: usize,
    seg_wait_len: usize,
    /// Per ring buffer.
    chunks: Box<[Chunk]>,
    /// Ring buffers to give back on the next `poll`.
    returns: Box<[BufId]>,
    returns_len: usize,
    /// Connections holding ring buffers they have not pulled yet.
    ring_pending: Box<[u32]>,
    ring_pending_len: usize,
    /// Free slots for connections the `Net` places itself.
    free: Box<[u32]>,
    free_len: usize,
    /// Connections first, then listeners.
    deadlines: Deadlines,
    /// Connections with events owed, as a ring.
    evq: Box<[u32]>,
    ev_head: usize,
    ev_len: usize,
    /// Connections with work for the next `poll`.
    work: Box<[u32]>,
    work_len: usize,
    /// The list `poll` is working through, so that work added meanwhile
    /// waits for the next turn.
    working: Box<[u32]>,
    turn: u32,
    now: u64,
    draining: Option<u64>,
    drained_owed: bool,
    drain_reported: bool,
    /// Connections with sends queued, or with the memory of a zero-copy
    /// send still out.
    tx_busy: u32,
    /// With TLS: the server name of each connection the caller opened.
    names: Box<[ServerName]>,
    /// Which cells of the state table were reached, in debug builds.
    coverage: Coverage,
}

impl<T: Transport> Net<T> {
    /// A `Net` on the share that `cfg` describes of a backend built with
    /// `limits`. All of its tables are allocated here; nothing is
    /// allocated afterwards.
    pub fn new(mut transport: T, cfg: Config, limits: &Limits) -> Net<T> {
        let cfg = cfg.resolved(limits);
        transport.attach(&cfg);
        let names = if cfg.sizes.tls {
            cfg.sizes.connects as usize
        } else {
            0
        };
        let s = cfg.sizes;
        let conns = cfg.conns();
        let opts = ConnOpts::new(Framing::Bytes);
        let quiet = TxSpan {
            buf: BufId::new(0),
            off: 0,
            len: 0,
            keep: Keep::Segment(0),
        };
        let unused = ListenerState::unused(opts);
        let chunk = Chunk {
            off: 0,
            len: 0,
            next: NONE,
        };
        Net {
            t: transport,
            cfg,
            gens: Generations::new(conns),
            lgens: Generations::new(s.listeners),
            conns: (0..conns)
                .map(|i| ConnState::free(opts, s.seed ^ u64::from(i)))
                .collect(),
            spans: vec![quiet; (conns * s.spans) as usize].into_boxed_slice(),
            listeners: vec![unused; s.listeners as usize].into_boxed_slice(),
            extents: Extents::new(cfg.frame_first, s.frames),
            zc: Pages::new(&cfg),
            segs: (0..s.segments).rev().collect(),
            segs_len: s.segments as usize,
            pins: vec![None; (conns * s.zero_copy_sends) as usize].into_boxed_slice(),
            pins_out: 0,
            seg_pins: vec![0; s.segments as usize].into_boxed_slice(),
            seg_parked: vec![false; s.segments as usize].into_boxed_slice(),
            seg_waiters: vec![0; conns as usize].into_boxed_slice(),
            seg_wait_head: 0,
            seg_wait_len: 0,
            chunks: vec![chunk; cfg.ring_count as usize].into_boxed_slice(),
            returns: vec![BufId::new(0); cfg.ring_count as usize].into_boxed_slice(),
            returns_len: 0,
            ring_pending: vec![0; conns as usize].into_boxed_slice(),
            ring_pending_len: 0,
            free: (0..s.connects).rev().collect(),
            free_len: s.connects as usize,
            deadlines: Deadlines::new(conns + s.listeners),
            evq: vec![0; conns as usize].into_boxed_slice(),
            ev_head: 0,
            ev_len: 0,
            work: vec![0; conns as usize].into_boxed_slice(),
            work_len: 0,
            working: vec![0; conns as usize].into_boxed_slice(),
            turn: 0,
            now: 0,
            draining: None,
            drained_owed: false,
            drain_reported: false,
            tx_busy: 0,
            names: vec![ServerName::NONE; names].into_boxed_slice(),
            coverage: Coverage::NONE,
        }
    }

    /// The transport it runs on, to read: what a transport counts or
    /// reports of itself, such as `KernelTransport::phase_coverage`.
    pub const fn transport(&self) -> &T {
        &self.t
    }

    /// The share of the backend this `Net` was built with.
    pub const fn config(&self) -> &Config {
        &self.cfg
    }

    /// Hold counts of frame memory: add a hold to keep a frame's buffer
    /// while a file write or anything else outside the `Net` uses it.
    pub fn extents(&mut self) -> &mut Extents {
        &mut self.extents
    }

    /// Holds of frames in pages: add a hold to a frame's run
    /// ([`Frame::run`]) while a file write reads its pages, and drop it
    /// when the write and its sync have completed.
    pub fn runs(&mut self) -> &mut Pages {
        &mut self.zc
    }

    /// Feed one completion of the driver. `false` when it is not the
    /// `Net`'s, and nothing was done.
    pub fn on_completion(&mut self, d: &mut T::Driver, completion: T::Completion) -> bool {
        let Some(happened) = self.t.translate(d, completion) else {
            return false;
        };
        match happened {
            Happened::Nothing => {}
            Happened::Connected { slot, result } => self.on_connected(d, slot, result),
            Happened::Secured { slot, result } => self.on_secured(d, slot, result),
            Happened::Accepted {
                listener,
                slot,
                armed,
            } => self.on_accepted(d, listener, slot, armed),
            Happened::AcceptFailed {
                listener,
                slot,
                result,
            } => self.on_accept_failed(listener, slot, result),
            Happened::RingData {
                slot,
                buf,
                len,
                armed,
            } => self.on_ring_data(d, slot, buf, len, armed),
            Happened::Piece {
                slot,
                page,
                off,
                len,
            } => self.on_piece(d, slot, zc::Piece { page, off, len }),
            Happened::RecvEnded { slot, result } => {
                if self.conns[slot as usize].opts.rx == Rx::ZeroCopy {
                    self.on_zc_ended(d, slot, result);
                } else {
                    self.on_recv_ended(slot, result);
                }
            }
            Happened::Received { slot, result } => self.on_received(d, slot, result),
            Happened::Sent { slot, result } => self.on_sent(d, slot, result, false),
            Happened::Taken { slot, result } => self.on_sent(d, slot, result, true),
            Happened::Released { slot, id } => self.on_released(slot, id),
            Happened::ShutDown { slot, result } => self.on_shut_down(slot, result),
            Happened::Closed { slot } => {
                self.cover(slot, cover::Col::Closed);
                self.conns[slot as usize].sock = Socket::Closed;
                self.try_free(slot);
            }
            Happened::ListenerClosed { listener } => self.on_listener_closed(listener),
        }
        true
    }

    /// Do what needs the driver and the time: give ring buffers back, fire
    /// deadlines, send what was queued, retry what the driver had no room
    /// for. Call once per turn, after pulling events and frames.
    pub fn poll(&mut self, d: &mut T::Driver, now_ns: u64) {
        self.now = now_ns;
        self.turn = self.turn.wrapping_add(1);
        self.spill_unpulled(d);
        for i in 0..self.returns_len {
            self.t.give_back(d, self.returns[i]);
        }
        self.returns_len = 0;
        self.return_pieces(d);
        self.wake_silent(d);
        let conns = self.cfg.conns();
        while let Some(slot) = self.deadlines.next_due(now_ns) {
            if slot < conns {
                self.on_conn_deadline(d, slot);
            } else {
                self.on_listener_deadline(d, slot - conns);
            }
        }
        for l in 0..self.cfg.sizes.listeners {
            self.start_accept(d, l);
        }
        core::mem::swap(&mut self.work, &mut self.working);
        let pending = core::mem::take(&mut self.work_len);
        for i in 0..pending {
            let slot = self.working[i];
            self.conns[slot as usize].working = false;
            self.do_work(d, slot);
        }
        // What this turn's work gave up goes back within the turn.
        self.return_pieces(d);
        if let Some(until) = self.draining {
            if !self.drain_reported && (self.tx_busy == 0 || now_ns >= until) {
                self.drain_reported = true;
                self.drained_owed = true;
            }
        }
    }

    /// The time at which `poll` must be called again at the latest, if
    /// there is one: the earliest connect, idle, reconnect or accept
    /// deadline.
    pub fn next_deadline(&mut self) -> Option<u64> {
        let drain = self.draining.filter(|_| !self.drain_reported);
        match (self.deadlines.earliest(), drain) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The next event, or `None` when there is none for now.
    pub fn next_event(&mut self) -> Option<Event> {
        if let Some(e) = self.listener_event() {
            return Some(e);
        }
        while self.ev_len > 0 {
            let slot = self.evq[self.ev_head];
            let c = &mut self.conns[slot as usize];
            if c.ev == 0 {
                c.queued = false;
                self.ev_head = (self.ev_head + 1) % self.evq.len();
                self.ev_len -= 1;
                continue;
            }
            let bit = 1 << c.ev.trailing_zeros();
            c.ev &= !bit;
            return Some(self.conn_event(slot, bit));
        }
        core::mem::take(&mut self.drained_owed).then_some(Event::Drained)
    }

    /// Owe the caller an event for connection `slot`. Only a connection
    /// the caller has a handle to, or is about to be given one for, is
    /// owed any: none that is closing, waiting to dial again, or free.
    fn queue_event(&mut self, slot: u32, bit: u8) {
        let c = &mut self.conns[slot as usize];
        let owed = matches!(
            c.st,
            St::Up | St::Refused | St::Finishing | St::HalfClosed | St::Dying
        );
        if !owed {
            return;
        }
        c.ev |= bit;
        if !c.queued {
            c.queued = true;
            let at = (self.ev_head + self.ev_len) % self.evq.len();
            self.evq[at] = slot;
            self.ev_len += 1;
        }
    }

    /// Something for the next `poll` to do for connection `slot`.
    fn add_work(&mut self, slot: u32, want: u8) {
        let c = &mut self.conns[slot as usize];
        c.want |= want;
        if !c.working {
            c.working = true;
            self.work[self.work_len] = slot;
            self.work_len += 1;
        }
    }

    /// Stop every `Up` connection from being told of an end twice.
    fn is_ending(&self, slot: u32) -> bool {
        // An end is owed only in `Dying`.
        let st = self.conns[slot as usize].st;
        matches!(st, St::Dying | St::Closing | St::Free | St::Backoff)
    }
}
