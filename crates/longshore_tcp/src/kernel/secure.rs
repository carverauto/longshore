// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS for the kernel transport: one `longshore_stream::Stream` per
//! connection slot, built once and keyed in place.
//!
//! A protected slot's sends are sealed into its own buffer and sent from
//! there; a receive into frame memory is opened there; bytes an armed
//! receive delivered are opened from the ring buffer into the memory `Net`
//! names. The handshake is rustls's, and is the one place that allocates.

use super::cover::PhaseCoverage;
use crate::config::Config;
use crate::transport::{Happened, Opened, Refused, Role, Span, TLS_FAILED};
use longshore_common::io::{BufId, Io, Op, Sock, Token, EBUSY, ECANCELED, ECONNRESET};
use longshore_stream::{RingBytes, RingDst, Step, Stream};
use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup};

const RECV: u64 = 0;
const SEND: u64 = 1;
/// Bytes at the end of a slot's receive buffer that its stream is not
/// told of: where a close alert is put together.
const ALERT_ROOM: u32 = 16;

/// Which sides of a TLS handshake a transport can take.
#[derive(Default)]
pub struct Tls {
    client: Option<ClientSetup>,
    server: Option<ServerSetup>,
}

impl Tls {
    /// Neither side yet.
    pub const fn new() -> Tls {
        Tls {
            client: None,
            server: None,
        }
    }

    /// Connections the caller opens verify the peer with `setup`.
    #[must_use]
    pub fn with_client(mut self, setup: ClientSetup) -> Tls {
        self.client = Some(setup);
        self
    }

    /// Accepted connections present the certificate of `setup`.
    #[must_use]
    pub fn with_server(mut self, setup: ServerSetup) -> Tls {
        self.server = Some(setup);
        self
    }
}

/// Where a slot's stream is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not protected.
    Plain,
    /// The handshake runs: one send or receive of the stream is in flight.
    Handshake,
    /// Closed during the handshake; its operation in flight has yet to end.
    Aborted,
    /// Protected.
    Secure,
    /// Protected, and the close alert is on its way out.
    Alerting,
    /// Closed while the close alert was on its way out.
    AlertAborted,
}

/// The TLS state of every connection slot.
pub(super) struct Secure {
    tls: Tls,
    /// One per slot once attached; empty without `Sizes::with_tls`.
    streams: Box<[Stream]>,
    phase: Box<[Phase]>,
    cover: PhaseCoverage,
    /// The receive in flight brings bytes for the stream to keep.
    kept: Box<[bool]>,
    /// The first of the slots' buffers, and what a stream is told of each.
    first: BufId,
    cap: u32,
}

impl Secure {
    pub(super) fn new(conns: u32) -> Secure {
        Secure {
            tls: Tls::new(),
            streams: Box::default(),
            phase: vec![Phase::Plain; conns as usize].into_boxed_slice(),
            cover: PhaseCoverage::NONE,
            kept: vec![false; conns as usize].into_boxed_slice(),
            first: BufId::new(0),
            cap: 0,
        }
    }

    pub(super) fn set(&mut self, tls: Tls) {
        self.tls = tls;
    }

    /// Build the streams over the buffers `config` reserves for them.
    pub(super) fn attach(&mut self, config: &Config, at: impl Fn(u32, u64) -> (Sock, Token)) {
        if !config.sizes.tls {
            return;
        }
        self.first = config.tls_first;
        self.cap = config.sizes.segment_size - ALERT_ROOM;
        self.streams = (0..config.conns())
            .map(|slot| {
                let ((sock, send), (_, recv)) = (at(slot, SEND), at(slot, RECV));
                let first = self.first.get() + 2 * slot;
                let (tx, rx) = (BufId::new(first), BufId::new(first + 1));
                // Nothing sealed is ever sent zero-copy: it is sealed anew
                // into the same buffer as soon as it has gone.
                Stream::new(sock, send, recv).with_tls_buffers(tx, rx, self.cap, self.cap, u32::MAX)
            })
            .collect();
    }

    pub(super) fn can(&self, client: bool) -> bool {
        let side = if client {
            self.tls.client.is_some()
        } else {
            self.tls.server.is_some()
        };
        side && !self.streams.is_empty()
    }

    pub(super) const fn coverage(&self) -> PhaseCoverage {
        self.cover
    }

    /// Count the event in column `event` of the phase table for `slot`.
    fn count(&mut self, slot: u32, event: usize) {
        let phase = match self.phase[slot as usize] {
            Phase::Plain => 0,
            Phase::Handshake => 1,
            Phase::Aborted => 2,
            Phase::Secure => 3,
            Phase::Alerting => 4,
            Phase::AlertAborted => 5,
        };
        self.cover.count(phase, event);
    }

    /// Whether `slot`'s sends and receives go through its stream.
    pub(super) fn is_on(&self, slot: u32) -> bool {
        self.phase[slot as usize] != Phase::Plain
    }

    pub(super) fn start<IO: Io>(
        &mut self,
        d: &mut IO,
        slot: u32,
        role: Role<'_>,
    ) -> Result<(), Refused> {
        self.count(slot, 0);
        let handshake = match (role, &self.tls.client, &self.tls.server) {
            (Role::Client { server_name }, Some(setup), _) => Handshake::client(setup, server_name),
            (Role::Server, _, Some(setup)) => Handshake::server(setup),
            (Role::Client { .. } | Role::Server, _, _) => return Err(Refused::Unsupported),
        };
        let (Ok(handshake), Some(stream)) = (handshake, self.streams.get_mut(slot as usize)) else {
            return Err(Refused::Unsupported);
        };
        stream.reset();
        if stream.start_handshake(d, handshake) != Step::Pending {
            stream.reset();
            return Err(Refused::Unsupported);
        }
        self.phase[slot as usize] = Phase::Handshake;
        Ok(())
    }

    pub(super) fn open<IO: Io>(
        &mut self,
        d: &mut IO,
        slot: u32,
        from: Span,
        into: Span,
    ) -> Option<Opened> {
        let stream = self.streams.get_mut(slot as usize)?;
        let bytes = RingBytes::new(from.buf(), from.off(), from.len());
        let dst = RingDst::new(into.buf(), into.off(), into.len());
        let step = stream.on_ring_recv(d, bytes, dst).ok()?;
        let opened = Opened::new(step.consumed(), step.produced(), step.closed());
        Some(opened.with_no_room(step.no_room()))
    }

    /// A receive into `into`; the result when plaintext was there already.
    pub(super) fn recv<IO: Io>(&mut self, d: &mut IO, slot: u32, into: Span) -> Option<i32> {
        let stream = &mut self.streams[slot as usize];
        match stream.recv(d, into.buf(), into.off(), into.len()) {
            Step::Pending => None,
            Step::Done(n) => Some(self.end_of_stream(slot, n)),
            Step::Tls(_) => Some(TLS_FAILED),
        }
    }

    /// A receive into the stream's own buffer; `false` when it is full.
    pub(super) fn recv_kept<IO: Io>(&mut self, d: &mut IO, slot: u32) -> bool {
        let started = self.streams[slot as usize].recv_carried(d);
        self.kept[slot as usize] = started;
        started
    }

    pub(super) fn send<IO: Io>(&mut self, d: &mut IO, slot: u32, from: Span) {
        self.streams[slot as usize].send(d, from.buf(), from.off(), from.len(), u32::MAX);
    }

    /// Send the close alert; the shutdown follows when it has gone.
    pub(super) fn say_goodbye<IO: Io>(&mut self, d: &mut IO, slot: u32) {
        self.count(slot, 2);
        let rx = BufId::new(self.first.get() + 2 * slot + 1);
        // Level 1 (warning), description 0 (close_notify).
        self.streams[slot as usize].send_alert(d, rx, self.cap, 1, 0);
        self.phase[slot as usize] = Phase::Alerting;
    }

    /// `slot` is being closed.
    pub(super) fn closing(&mut self, slot: u32) {
        self.count(slot, 3);
        let phase = &mut self.phase[slot as usize];
        *phase = match *phase {
            Phase::Handshake => Phase::Aborted,
            Phase::Alerting => Phase::AlertAborted,
            Phase::Plain | Phase::Aborted | Phase::Secure | Phase::AlertAborted => *phase,
        };
    }

    /// `slot` is closed: what is still in flight ends as on a plain slot.
    pub(super) fn closed(&mut self, slot: u32) {
        self.count(slot, 4);
        if self.phase[slot as usize] == Phase::Secure {
            self.phase[slot as usize] = Phase::Plain;
            self.streams[slot as usize].reset();
        }
    }

    /// An end of stream without a close alert was cut short.
    pub(super) fn end_of_stream(&self, slot: u32, result: i32) -> i32 {
        let cut = result == 0 && self.is_on(slot) && !self.streams[slot as usize].peer_closed();
        if cut {
            -ECONNRESET
        } else {
            result
        }
    }

    /// A send (`sent`) or receive completion of a slot that is not plain.
    /// `shutdown` is what follows a close alert.
    pub(super) fn on_io<IO: Io>(
        &mut self,
        d: &mut IO,
        slot: u32,
        sent: bool,
        result: i32,
        shutdown: (Token, Op),
    ) -> Happened {
        let i = slot as usize;
        self.count(slot, 1);
        let kept = !sent && core::mem::take(&mut self.kept[i]);
        if kept && matches!(self.phase[i], Phase::Secure | Phase::Alerting) {
            let result = self.streams[i].on_carried(result);
            return Happened::Received { slot, result };
        }
        match (self.phase[i], sent) {
            (Phase::Plain, true) => Happened::Sent { slot, result },
            (Phase::Plain | Phase::AlertAborted, false) => Happened::Received { slot, result },
            (Phase::Handshake, _) => self.on_handshake_io(d, slot, sent, result),
            (Phase::Aborted, _) => {
                self.streams[i].reset();
                self.phase[i] = Phase::Plain;
                Happened::Secured {
                    slot,
                    result: -ECANCELED,
                }
            }
            (Phase::Secure, true) => match self.streams[i].on_send(d, result) {
                Step::Pending => Happened::Nothing,
                Step::Done(result) => Happened::Sent { slot, result },
                Step::Tls(_) => Happened::Sent {
                    slot,
                    result: TLS_FAILED,
                },
            },
            (Phase::Secure | Phase::Alerting, false) => match self.streams[i].on_recv(d, result) {
                Step::Pending => Happened::Nothing,
                Step::Done(n) => Happened::Received {
                    slot,
                    result: self.end_of_stream(slot, n),
                },
                Step::Tls(_) => Happened::Received {
                    slot,
                    result: TLS_FAILED,
                },
            },
            (Phase::Alerting, true) => {
                if self.streams[i].on_send(d, result) == Step::Pending {
                    return Happened::Nothing;
                }
                self.phase[i] = Phase::Secure;
                match d.submit(shutdown.0, shutdown.1) {
                    Ok(()) => Happened::Nothing,
                    Err(_) => Happened::ShutDown {
                        slot,
                        result: -EBUSY,
                    },
                }
            }
            (Phase::AlertAborted, true) => {
                self.streams[i].reset();
                self.phase[i] = Phase::Plain;
                Happened::ShutDown {
                    slot,
                    result: -ECANCELED,
                }
            }
        }
    }

    /// A completion of the handshake of `slot`.
    fn on_handshake_io<IO: Io>(
        &mut self,
        d: &mut IO,
        slot: u32,
        sent: bool,
        result: i32,
    ) -> Happened {
        let i = slot as usize;
        let result = match self.streams[i].on_handshake(d, sent, result) {
            Step::Pending => return Happened::Nothing,
            Step::Done(r) => r,
            Step::Tls(_) => TLS_FAILED,
        };
        self.phase[i] = if result == 0 {
            Phase::Secure
        } else {
            self.streams[i].reset();
            Phase::Plain
        };
        Happened::Secured { slot, result }
    }
}
