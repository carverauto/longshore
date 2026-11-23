// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the resolver tests: a simulated nameserver with A and AAAA
//! records, and the loop that drives it with allocation sealed.
#![expect(
    dead_code,
    reason = "shared by the resolver test binaries; each binary uses only part of it"
)]

pub use longshore_common::io::alloc_guard::{sealed, Guard};
pub use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
pub use longshore_common::sim::{Faults, SimIo};
pub use longshore_dns::{Config, Family, Resolver};
pub use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

pub const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_buffers(4)
    .with_buffer_size(1024)
    .with_ops_max(8);
pub const NS1: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 53)), 53);
pub const NS2: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 54)), 53);
pub const MS: u64 = 1_000_000;
pub const SEC: u64 = 1000 * MS;

/// Name, A records, and the last 16 bits of each AAAA record (the rest is
/// `2001:db8::`).
pub type Entry = (&'static str, &'static [[u8; 4]], &'static [u16]);

pub const ZONE: [Entry; 2] = [
    (
        "s3.example.com",
        &[[10, 1, 0, 1], [10, 1, 0, 2], [10, 1, 0, 3]],
        &[0x101, 0x102],
    ),
    ("blob.example.net", &[[10, 2, 0, 1]], &[0x201]),
];

/// Simulated nameserver on socket 3 / buffer 3. Answers in place, no allocation.
pub struct Server {
    answering: bool,
    /// Whether the zone has records of each family. Without them a query
    /// for that family gets an empty answer.
    v4: bool,
    v6: bool,
    ttl: u32,
    queries: u64,
    /// What is wrong with each response, if anything.
    forge: Forge,
    /// After a forged response, send the true one for the same query.
    then_true: bool,
    /// The length of the true response still to send.
    resend: Option<u32>,
}

/// A way for a response to be something other than the answer to the
/// query: what an off-path attacker, a confused middlebox or a stale
/// datagram would deliver. Each is undone by applying it again.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Forge {
    Nothing,
    /// Another transaction id.
    WrongId,
    /// A question for another name.
    WrongQuestion,
    /// The response bit is clear: the datagram is a query.
    NotAResponse,
    /// The last byte is missing.
    CutShort,
    /// The server says the answer did not fit (the TC bit).
    TcBit,
}

impl Forge {
    fn apply(self, m: &mut [u8]) {
        match self {
            Forge::Nothing | Forge::CutShort => {}
            Forge::WrongId => m[1] ^= 1,
            // The first letter of the name: `s` becomes `r`, `b` becomes `c`.
            Forge::WrongQuestion => m[13] ^= 1,
            Forge::NotAResponse => m[2] ^= 0x80,
            Forge::TcBit => m[2] ^= 0x02,
        }
    }

    fn len(self, true_len: usize) -> u32 {
        (true_len - usize::from(self == Forge::CutShort)) as u32
    }
}

const S_SOCK: Sock = Sock::new(3);
const S_BUF: BufId = BufId::new(3);
const S_RECV: u64 = 1;
const S_SEND: u64 = 2;

impl Server {
    pub fn start(io: &mut SimIo, addr: SocketAddr) -> Server {
        io.bind_udp(S_SOCK, addr);
        io.submit(Token::new(S_RECV), RECV).unwrap();
        Server {
            answering: true,
            v4: true,
            v6: true,
            ttl: 300,
            queries: 0,
            forge: Forge::Nothing,
            then_true: false,
            resend: None,
        }
    }

    /// Queries received.
    pub fn queries(&self) -> u64 {
        self.queries
    }

    /// Whether queries are answered at all.
    pub fn set_answering(&mut self, answering: bool) {
        self.answering = answering;
    }

    /// Whether the zone has A records.
    pub fn set_v4(&mut self, v4: bool) {
        self.v4 = v4;
    }

    /// Whether the zone has AAAA records.
    pub fn set_v6(&mut self, v6: bool) {
        self.v6 = v6;
    }

    pub fn set_ttl(&mut self, ttl: u32) {
        self.ttl = ttl;
    }

    /// What is wrong with each response, and whether the true one follows a
    /// forged one.
    pub fn set_forge(&mut self, forge: Forge, then_true: bool) {
        self.forge = forge;
        self.then_true = then_true;
    }

    pub fn on(&mut self, io: &mut SimIo, token: u64, result: i32) {
        if token == S_SEND {
            return self.on_sent(io);
        }
        self.queries += 1;
        let n = usize::try_from(result).unwrap();
        if !self.answering {
            return io.submit(Token::new(S_RECV), RECV).unwrap();
        }
        let m = io.buf(S_BUF);
        let at = self.answer(m, n);
        self.forge.apply(m);
        if self.then_true && self.forge != Forge::Nothing {
            self.resend = Some(at as u32);
        }
        let send = Op::Send {
            sock: S_SOCK,
            buf: S_BUF,
            off: 0,
            len: self.forge.len(at),
            zero_copy: false,
        };
        io.submit(Token::new(S_SEND), send).unwrap();
    }

    /// A response has gone: listen again, or send the true response
    /// after a forged one.
    fn on_sent(&mut self, io: &mut SimIo) {
        let Some(len) = self.resend.take() else {
            return io.submit(Token::new(S_RECV), RECV).unwrap();
        };
        // The forged response has gone; now the true one.
        self.forge.apply(io.buf(S_BUF));
        let send = Op::Send {
            sock: S_SOCK,
            buf: S_BUF,
            off: 0,
            len,
            zero_copy: false,
        };
        io.submit(Token::new(S_SEND), send).unwrap();
    }

    /// Turns the `n`-byte query in `m` into its answer in place. Returns
    /// the answer's length.
    fn answer(&self, m: &mut [u8], n: usize) -> usize {
        let ttl = self.ttl.to_be_bytes();
        let entry = zone_entry(m);
        // The question's type: 1 for A, 28 for AAAA.
        let aaaa = m[n - 3] == 28;
        m[2] = 0x81;
        m[3] = if entry.is_some() { 0x80 } else { 0x83 };
        let mut at = n;
        let mut count = 0;
        let mut record = |msg: &mut [u8], rtype: u8, data: &[u8]| {
            msg[at..at + 4].copy_from_slice(&[0xc0, 12, 0, rtype]);
            msg[at + 4..at + 6].copy_from_slice(&[0, 1]);
            msg[at + 6..at + 10].copy_from_slice(&ttl);
            msg[at + 10..at + 12].copy_from_slice(&(data.len() as u16).to_be_bytes());
            msg[at + 12..at + 12 + data.len()].copy_from_slice(data);
            at += 12 + data.len();
            count += 1;
        };
        if let Some((_, a, quad)) = entry {
            if aaaa && self.v6 {
                for tail in *quad {
                    record(m, 28, &zone_v6(*tail).octets());
                }
            } else if !aaaa && self.v4 {
                for addr in *a {
                    record(m, 1, addr);
                }
            }
        }
        m[7] = count;
        at
    }
}

const RECV: Op = Op::Recv {
    sock: S_SOCK,
    buf: S_BUF,
    off: 0,
    len: 512,
};

/// The zone entry the query in `m` asks about. The question name runs
/// from byte 12 to 5 bytes before the end.
fn zone_entry(m: &[u8]) -> Option<&'static Entry> {
    ZONE.iter().find(|(name, _, _)| {
        let mut at = 12;
        name.split('.').all(|l| {
            let ok = m[at] as usize == l.len()
                && m[at + 1..at + 1 + l.len()].eq_ignore_ascii_case(l.as_bytes());
            at += 1 + l.len();
            ok
        })
    })
}

fn zone_v6(tail: u16) -> Ipv6Addr {
    Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, tail)
}

pub fn config(servers: &[SocketAddr], refresh_ns: u64, family: Family) -> Config {
    Config::new(
        servers.to_vec(),
        ZONE.iter().map(|z| z.0.to_string()).collect(),
        [Sock::new(0), Sock::new(1)],
        [BufId::new(0), BufId::new(1)],
        BufId::new(2),
        1 << 40,
    )
    .with_family(family)
    .with_prefer_large_segments(None)
    .with_refresh_ns(refresh_ns)
    .with_timeout_ns(50 * MS)
}

/// Run resolver and server until virtual time `until`, with allocation forbidden.
pub fn pump(io: &mut SimIo, r: &mut Resolver, s: &mut Server, out: &mut Completions, until: u64) {
    sealed(|| {
        while io.now_ns() < until {
            io.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if !r.on_completion(io, c) {
                    s.on(io, c.token().get(), c.result());
                }
            }
        }
    })
}

/// A resolver for IPv4 only, with a nameserver bound at `bind`.
pub fn world(
    seed: u64,
    faults: Faults,
    servers: &[SocketAddr],
    bind: SocketAddr,
    refresh: u64,
) -> (SimIo, Resolver, Server, Completions) {
    world_for(seed, faults, servers, bind, refresh, Family::V4)
}

pub fn world_for(
    seed: u64,
    faults: Faults,
    servers: &[SocketAddr],
    bind: SocketAddr,
    refresh: u64,
    family: Family,
) -> (SimIo, Resolver, Server, Completions) {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let s = Server::start(&mut io, bind);
    let mut r = Resolver::new(config(servers, refresh, family)).expect("resolver config");
    r.start(&mut io);
    (io, r, s, Completions::new(8))
}

/// The zone's A records for host `i`.
pub fn zone_addrs(i: usize) -> Vec<IpAddr> {
    ZONE[i].1.iter().map(|a| IpAddr::from(*a)).collect()
}

/// The zone's AAAA records for host `i`.
pub fn zone_addrs_v6(i: usize) -> Vec<IpAddr> {
    ZONE[i].2.iter().map(|t| IpAddr::V6(zone_v6(*t))).collect()
}
