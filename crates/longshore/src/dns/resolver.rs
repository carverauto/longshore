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

use crate::dns::codec::{encode_query, parse_response, DnsError, ADDRS_MAX, MESSAGE_MAX};
use crate::io::{BufId, Completion, Io, Op, Sock, Token};
use std::net::{Ipv4Addr, SocketAddrV4};

pub const SERVERS_MAX: usize = 2;

/// Operations the resolver keeps in flight at most: one receive per server,
/// one send, one timer. Connects happen before any of those.
pub const OPS_IN_FLIGHT_MAX: u32 = SERVERS_MAX as u32 + 2;

pub struct Config {
    /// One or two nameservers. On a timeout the next one is tried.
    pub servers: Vec<SocketAddrV4>,
    pub hosts: Vec<String>,
    /// Every host is re-resolved this often, whatever its TTL says.
    pub refresh_ns: u64,
    /// How long to wait for one answer.
    pub timeout_ns: u64,
    /// One socket slot and one receive buffer per server.
    pub socks: [Sock; SERVERS_MAX],
    pub rx_bufs: [BufId; SERVERS_MAX],
    pub tx_buf: BufId,
    /// Tokens used are `token_base..token_base + 256`.
    pub token_base: u64,
}

struct Host {
    name: String,
    addrs: [Ipv4Addr; ADDRS_MAX],
    len: usize,
    next: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Connecting {
        remaining: usize,
    },
    /// Waiting for the answer for `host`; `attempt` counts servers tried.
    Querying {
        host: usize,
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
    /// Lookups that failed or timed out, and receive errors.
    pub failures: u64,
    /// Queries sent.
    pub queries: u64,
    /// Completed passes over all hosts.
    pub cycles: u64,
}

impl Resolver {
    pub fn new(cfg: Config) -> Resolver {
        assert!(
            (1..=SERVERS_MAX).contains(&cfg.servers.len()),
            "one or two nameservers"
        );
        assert!(!cfg.hosts.is_empty(), "at least one hostname");
        assert!(cfg.timeout_ns > 0 && cfg.refresh_ns > 0);
        let mut probe = [0u8; MESSAGE_MAX];
        let hosts = cfg
            .hosts
            .iter()
            .map(|h| {
                encode_query(h, 0, &mut probe).unwrap_or_else(|_| panic!("invalid hostname {h:?}"));
                Host {
                    name: h.clone(),
                    addrs: [Ipv4Addr::UNSPECIFIED; ADDRS_MAX],
                    len: 0,
                    next: 0,
                }
            })
            .collect();
        Resolver {
            cfg,
            hosts,
            phase: Phase::Connecting { remaining: 0 },
            server: 0,
            id: 0,
            rng: 0,
            send_in_flight: false,
            send_wanted: false,
            failures: 0,
            queries: 0,
            cycles: 0,
        }
    }

    /// Begin resolving. Call once.
    pub fn start<IO: Io>(&mut self, io: &mut IO) {
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

    /// True once every hostname has resolved at least once.
    pub fn ready(&self) -> bool {
        self.hosts.iter().all(|h| h.len > 0)
    }

    pub fn host_index(&self, name: &str) -> Option<usize> {
        fn bare(n: &str) -> &[u8] {
            n.strip_suffix('.').unwrap_or(n).as_bytes()
        }
        let name = bare(name);
        self.hosts
            .iter()
            .position(|h| bare(&h.name).eq_ignore_ascii_case(name))
    }

    /// Next address for a host, rotating through its records. No I/O.
    pub fn lookup(&mut self, host: usize) -> Option<Ipv4Addr> {
        let h = &mut self.hosts[host];
        if h.len == 0 {
            return None;
        }
        h.next = (h.next + 1) % h.len;
        Some(h.addrs[h.next])
    }

    /// All cached addresses for a host.
    pub fn addrs(&self, host: usize) -> &[Ipv4Addr] {
        &self.hosts[host].addrs[..self.hosts[host].len]
    }

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

    fn query<IO: Io>(&mut self, io: &mut IO, host: usize, attempt: usize) {
        self.phase = Phase::Querying {
            host,
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
        let len = encode_query(&self.hosts[host].name, self.id, io.buf(self.cfg.tx_buf))
            .expect("hostnames were validated in new()");
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

    /// Move to the next host, or finish the cycle.
    fn advance<IO: Io>(&mut self, io: &mut IO, host: usize) {
        if host + 1 < self.hosts.len() {
            self.query(io, host + 1, 0);
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
        match kind {
            k if k < K_RECV => {
                let server = (k - K_CONNECT) as usize;
                if c.result < 0 {
                    self.failures += 1; // queries to this server will time out
                } else {
                    self.recv(io, server);
                }
                if let Phase::Connecting { remaining } = self.phase {
                    if remaining == 1 {
                        Self::submit(
                            io,
                            self.cfg.token_base + K_TICK,
                            Op::Timeout {
                                nanos: self.tick_period(),
                            },
                        );
                        self.query(io, 0, 0);
                    } else {
                        self.phase = Phase::Connecting {
                            remaining: remaining - 1,
                        };
                    }
                }
            }
            k if k < K_SEND => {
                let server = (k - K_RECV) as usize;
                if c.result < 0 {
                    self.failures += 1;
                } else if let Phase::Querying { host, .. } = self.phase {
                    let msg = &io.buf(self.cfg.rx_bufs[server])[..c.result as usize];
                    match parse_response(msg, self.id, &self.hosts[host].name) {
                        Ok(a) => {
                            let h = &mut self.hosts[host];
                            h.addrs = a.addrs;
                            h.len = a.len;
                            h.next %= a.len;
                            self.advance(io, host);
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
                            self.advance(io, host);
                        }
                    }
                }
                self.recv(io, server);
            }
            K_SEND => {
                self.send_in_flight = false;
                if c.result < 0 {
                    self.failures += 1;
                }
                if self.send_wanted {
                    self.send_wanted = false;
                    if let Phase::Querying { host, attempt, .. } = self.phase {
                        self.query(io, host, attempt);
                    }
                }
            }
            K_TICK => {
                let now = io.now_ns();
                match self.phase {
                    Phase::Querying {
                        host,
                        attempt,
                        deadline,
                    } if now >= deadline => {
                        self.failures += 1;
                        self.server = (self.server + 1) % self.cfg.servers.len();
                        if attempt + 1 < self.cfg.servers.len() {
                            self.query(io, host, attempt + 1);
                        } else {
                            self.advance(io, host);
                        }
                    }
                    Phase::Idle { at } if now >= at => self.query(io, 0, 0),
                    _ => {}
                }
                Self::submit(
                    io,
                    self.cfg.token_base + K_TICK,
                    Op::Timeout {
                        nanos: self.tick_period(),
                    },
                );
            }
            _ => {}
        }
        true
    }
}
