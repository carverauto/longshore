// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Resolver for a fixed set of hostnames, driven by `Io` completions.
//!
//! The owner routes completions to [`Resolver::on_completion`]. Lookups read
//! the cache and take no `Io`, so the request path cannot block on DNS.
//!
//! Pace is set by the resolver, not by the network: one timer ticks at a fixed
//! period, query deadlines and the refresh interval are checked on that tick,
//! and record TTLs are ignored.
//!
//! A host has one address set per family. [`Family`] says which record
//! types are asked for and which family lookups hand out first.

use crate::codec::{encode_query_for, MESSAGE_MAX};
use longshore_common::io::{Completion, Io, Op, Token};

mod config;
mod events;
mod family;
mod lookup;
pub use config::{Config, ConfigError};
pub use family::Family;
use family::{set_of, Host, Set};

pub const SERVERS_MAX: usize = 2;

/// Operations the resolver keeps in flight at most: one receive per server,
/// one send, one timer. Connects happen before any of those.
pub const OPS_IN_FLIGHT_MAX: u32 = SERVERS_MAX as u32 + 2;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Connecting {
        remaining: usize,
    },
    /// Waiting for the answer to question `q` of `host` (an index into the
    /// family's records); `attempt` counts servers tried.
    Querying {
        host: usize,
        q: usize,
        attempt: usize,
        deadline: u64,
    },
    /// All hosts done for this cycle; the next starts at `at`.
    Idle {
        at: u64,
    },
}

const K_CONNECT: u64 = 0; // + server index
const K_RECV: u64 = 8; // + server index
const K_SEND: u64 = 16;
const K_TICK: u64 = 17;

pub struct Resolver {
    cfg: Config,
    hosts: Vec<Host>,
    phase: Phase,
    server: usize,
    id: u16,
    rng: u64,
    send_in_flight: bool,
    /// A query is waiting for the previous send to release the transmit buffer.
    send_wanted: bool,
    /// Families that answered with no addresses for the host being asked.
    empty: [bool; 2],
    failures: u64,
    queries: u64,
    cycles: u64,
    family_switches: u64,
    early_refreshes: u64,
}

impl Resolver {
    /// A resolver for `cfg`, or why `cfg` cannot be used.
    pub fn new(cfg: Config) -> Result<Resolver, ConfigError> {
        cfg.check()?;
        let preferred = cfg.family.records()[0];
        let hosts = cfg
            .hosts
            .iter()
            .map(|h| Host {
                name: h.clone(),
                sets: [Set::EMPTY; 2],
                active: set_of(preferred),
                path_limit: 0,
            })
            .collect();
        Ok(Resolver {
            cfg,
            hosts,
            phase: Phase::Connecting { remaining: 0 },
            server: 0,
            id: 0,
            rng: 0,
            send_in_flight: false,
            send_wanted: false,
            empty: [false; 2],
            failures: 0,
            queries: 0,
            cycles: 0,
            family_switches: 0,
            early_refreshes: 0,
        })
    }

    /// Lookups that failed or timed out, and receive errors.
    #[inline]
    #[must_use]
    pub const fn failures(&self) -> u64 {
        self.failures
    }

    /// Queries sent.
    #[inline]
    #[must_use]
    pub const fn queries(&self) -> u64 {
        self.queries
    }

    /// Completed passes over all hosts.
    #[inline]
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Times a host's lookups moved to the other family after a failed
    /// connection.
    #[inline]
    #[must_use]
    pub const fn family_switches(&self) -> u64 {
        self.family_switches
    }

    /// Refreshes started early because a host had only small-segment
    /// addresses.
    #[inline]
    #[must_use]
    pub const fn early_refreshes(&self) -> u64 {
        self.early_refreshes
    }

    /// Begin resolving. Call once.
    pub fn start<IO: Io>(&mut self, io: &mut IO) {
        // The buffers are the caller's. Too small a one would panic at the
        // first answer or query that did not fit; say so now, by name.
        for (i, _) in self.cfg.servers.iter().enumerate() {
            assert!(
                io.buf(self.cfg.rx_bufs[i]).len() >= MESSAGE_MAX,
                "rx_bufs[{i}] must hold {MESSAGE_MAX} bytes"
            );
        }
        let record = self.cfg.family.records()[0];
        for host in &self.hosts {
            assert!(
                encode_query_for(&host.name, 0, record, io.buf(self.cfg.tx_buf)).is_ok(),
                "tx_buf is too small for a query for {:?}",
                host.name
            );
        }
        self.rng = io.now_ns() ^ self.cfg.token_base ^ 0x9e37_79b9_7f4a_7c15;
        self.phase = Phase::Connecting {
            remaining: self.cfg.servers.len(),
        };
        for (i, addr) in self.cfg.servers.iter().enumerate() {
            Self::submit(
                io,
                self.cfg.token_base + K_CONNECT + i as u64,
                Op::ConnectUdp {
                    sock: self.cfg.socks[i],
                    addr: *addr,
                },
            );
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "the resolver keeps at most OPS_IN_FLIGHT_MAX operations, which Limits::ops_max must leave it"
    )]
    fn submit<IO: Io>(io: &mut IO, token: u64, op: Op) {
        io.submit(Token(token), op)
            .expect("Limits::ops_max must leave OPS_IN_FLIGHT_MAX slots for the resolver");
    }

    fn tick_period(&self) -> u64 {
        (self.cfg.timeout_ns.min(self.cfg.refresh_ns) / 4).max(1)
    }

    fn recv<IO: Io>(&self, io: &mut IO, server: usize) {
        Self::submit(
            io,
            self.cfg.token_base + K_RECV + server as u64,
            Op::Recv {
                sock: self.cfg.socks[server],
                buf: self.cfg.rx_bufs[server],
                off: 0,
                len: MESSAGE_MAX as u32,
            },
        );
    }

    fn query<IO: Io>(&mut self, io: &mut IO, host: usize, q: usize, attempt: usize) {
        self.phase = Phase::Querying {
            host,
            q,
            attempt,
            deadline: io.now_ns() + self.cfg.timeout_ns,
        };
        if self.send_in_flight {
            self.send_wanted = true;
            return;
        }
        // Query ids only need to be hard to guess, not cryptographically random.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.id = (self.rng >> 24) as u16;
        let record = self.cfg.family.records()[q];
        #[expect(
            clippy::expect_used,
            reason = "new() validated every hostname and start() checked tx_buf holds its query"
        )]
        let len = encode_query_for(
            &self.hosts[host].name,
            self.id,
            record,
            io.buf(self.cfg.tx_buf),
        )
        .expect("hostnames and tx_buf were checked before the first query");
        self.send_in_flight = true;
        self.queries += 1;
        Self::submit(
            io,
            self.cfg.token_base + K_SEND,
            Op::Send {
                sock: self.cfg.socks[self.server],
                buf: self.cfg.tx_buf,
                off: 0,
                len: len as u32,
                zero_copy: false,
            },
        );
    }

    /// Move to the host's next question, the next host, or finish the cycle.
    /// After a host's last question, its empty answers are applied.
    fn advance<IO: Io>(&mut self, io: &mut IO, host: usize, q: usize) {
        if q + 1 < self.cfg.family.records().len() {
            return self.query(io, host, q + 1, 0);
        }
        if self.hosts[host].drop_empty(std::mem::take(&mut self.empty)) {
            self.failures += 1;
        }
        if host + 1 < self.hosts.len() {
            self.query(io, host + 1, 0, 0);
        } else {
            self.cycles += 1;
            self.phase = Phase::Idle {
                at: io.now_ns() + self.cfg.refresh_ns,
            };
        }
    }

    /// Feed a completion. Returns false if the token is not the resolver's.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> bool {
        let Some(kind) = c
            .token
            .0
            .checked_sub(self.cfg.token_base)
            .filter(|k| *k < 256)
        else {
            return false;
        };
        // The range leaves room for eight servers and there are at most two.
        // A completion naming one that does not exist is nobody's: ignore it.
        if kind < K_SEND && (kind % K_RECV) as usize >= self.cfg.servers.len() {
            return true;
        }
        match kind {
            k if k < K_RECV => self.on_connect(io, (k - K_CONNECT) as usize, c.result),
            k if k < K_SEND => self.on_recv(io, (k - K_RECV) as usize, c.result),
            K_SEND => self.on_send(io, c.result),
            K_TICK => self.on_tick(io),
            _ => {}
        }
        true
    }
}
