// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One handler per kind of completion the resolver submits: connects,
//! receives, sends and the tick.

use super::{set_of, Phase, Resolver, K_TICK};
use crate::codec::{parse_response_for, DnsError};
use longshore_common::io::{Io, Op};

impl Resolver {
    /// A server's socket connected (or failed to). The last connect starts
    /// the tick and the first query.
    pub(super) fn on_connect<IO: Io>(&mut self, io: &mut IO, server: usize, result: i32) {
        if result < 0 {
            self.failures += 1; // queries to this server will time out
        } else {
            self.recv(io, server);
        }
        if let Phase::Connecting { remaining } = self.phase {
            if remaining == 1 {
                self.submit_tick(io);
                self.query(io, 0, 0, 0);
            } else {
                self.phase = Phase::Connecting {
                    remaining: remaining - 1,
                };
            }
        }
    }

    /// A datagram (or an error) arrived from `server`; receive again after.
    pub(super) fn on_recv<IO: Io>(&mut self, io: &mut IO, server: usize, result: i32) {
        if let Ok(len) = usize::try_from(result) {
            if let Phase::Querying { host, q, .. } = self.phase {
                self.on_answer(io, server, len, host, q);
            }
        } else {
            self.failures += 1;
        }
        self.recv(io, server);
    }

    /// Apply the `len` bytes `server` sent as an answer to question `q` of `host`.
    fn on_answer<IO: Io>(&mut self, io: &mut IO, server: usize, len: usize, host: usize, q: usize) {
        let records = self.cfg.family.records();
        let record = records[q];
        let msg = &io.buf(self.cfg.rx_bufs[server])[..len];
        match parse_response_for(msg, self.id, &self.hosts[host].name, record) {
            Ok(a) => {
                self.hosts[host].sets[set_of(record)].replace(a.as_slice());
                self.advance(io, host, q);
            }
            // The name has no records of this family. With two
            // families that is an answer, not a failure; it is
            // applied once the host's last question is answered.
            Err(DnsError::NoAddresses) if records.len() > 1 => {
                self.empty[set_of(record)] = true;
                self.advance(io, host, q);
            }
            // Not an answer to the outstanding query: ignore it.
            Err(
                DnsError::IdMismatch
                | DnsError::QuestionMismatch
                | DnsError::NotAResponse
                | DnsError::Truncated
                | DnsError::BadCompression,
            ) => {}
            // A definite answer we cannot use. Keep the previous addresses.
            Err(_) => {
                self.failures += 1;
                self.advance(io, host, q);
            }
        }
    }

    /// The transmit buffer is free again; send a query that waited for it.
    pub(super) fn on_send<IO: Io>(&mut self, io: &mut IO, result: i32) {
        self.send_in_flight = false;
        if result < 0 {
            self.failures += 1;
        }
        if self.send_wanted {
            self.send_wanted = false;
            if let Phase::Querying {
                host, q, attempt, ..
            } = self.phase
            {
                self.query(io, host, q, attempt);
            }
        }
    }

    /// Check the query deadline and the refresh interval, then tick again.
    pub(super) fn on_tick<IO: Io>(&mut self, io: &mut IO) {
        let now = io.now_ns();
        match self.phase {
            Phase::Querying {
                host,
                q,
                attempt,
                deadline,
            } if now >= deadline => {
                self.failures += 1;
                self.server = (self.server + 1) % self.cfg.servers.len();
                if attempt + 1 < self.cfg.servers.len() {
                    self.query(io, host, q, attempt + 1);
                } else {
                    self.advance(io, host, q);
                }
            }
            Phase::Idle { at } if now >= at => self.query(io, 0, 0, 0),
            // Every address of some host has small segments for its
            // path: ask again sooner, one answer timeout after the
            // last cycle, in case the name now has better ones.
            Phase::Idle { at }
                if self.starved() && now + self.cfg.refresh_ns >= at + self.cfg.timeout_ns =>
            {
                self.early_refreshes += 1;
                self.query(io, 0, 0, 0)
            }
            Phase::Connecting { .. } | Phase::Querying { .. } | Phase::Idle { .. } => {}
        }
        self.submit_tick(io);
    }

    fn submit_tick<IO: Io>(&self, io: &mut IO) {
        Self::submit(
            io,
            self.cfg.token_base + K_TICK,
            Op::Timeout {
                nanos: self.tick_period(),
            },
        );
    }
}
