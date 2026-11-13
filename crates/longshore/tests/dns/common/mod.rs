// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the resolver tests: a simulated nameserver with A and AAAA
//! records, and the loop that drives it with allocation sealed.
#![allow(dead_code)]

pub use longshore::dns::{Config, Family, Resolver};
pub use longshore::io::alloc_guard::{sealed, Guard};
pub use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
pub use longshore::sim::{Faults, SimIo};
pub use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

pub const LIMITS: Limits = Limits {
    sockets_max: 4,
    buffers: 4,
    buffer_size: 1024,
    small_buffers: 0,
    small_buffer_size: 0,
    ops_max: 8,
};
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
    pub answering: bool,
    /// Whether the zone has records of each family. Without them a query
    /// for that family gets an empty answer.
    pub v4: bool,
    pub v6: bool,
    pub ttl: u32,
    pub queries: u64,
}

const S_SOCK: Sock = Sock(3);
const S_BUF: BufId = BufId(3);
const S_RECV: u64 = 1;
const S_SEND: u64 = 2;

impl Server {
    pub fn start(io: &mut SimIo, addr: SocketAddr) -> Server {
        io.bind_udp(S_SOCK, addr);
        io.submit(
            Token(S_RECV),
            Op::Recv {
                sock: S_SOCK,
                buf: S_BUF,
                off: 0,
                len: 512,
            },
        )
        .unwrap();
        Server {
            answering: true,
            v4: true,
            v6: true,
            ttl: 300,
            queries: 0,
        }
    }

    pub fn on(&mut self, io: &mut SimIo, token: u64, result: i32) {
        let recv = Op::Recv {
            sock: S_SOCK,
            buf: S_BUF,
            off: 0,
            len: 512,
        };
        if token == S_SEND {
            return io.submit(Token(S_RECV), recv).unwrap();
        }
        self.queries += 1;
        let n = result as usize;
        let ttl = self.ttl.to_be_bytes();
        let m = io.buf(S_BUF);
        // The question name runs from byte 12 to 5 bytes before the end.
        let entry = ZONE.iter().find(|(name, _, _)| {
            let mut at = 12;
            name.split('.').all(|l| {
                let ok = m[at] as usize == l.len()
                    && m[at + 1..at + 1 + l.len()].eq_ignore_ascii_case(l.as_bytes());
                at += 1 + l.len();
                ok
            })
        });
        if !self.answering {
            return io.submit(Token(S_RECV), recv).unwrap();
        }
        // The question's type: 1 for A, 28 for AAAA.
        let aaaa = m[n - 3] == 28;
        m[2] = 0x81;
        m[3] = if entry.is_some() { 0x80 } else { 0x83 };
        let mut at = n;
        let mut count = 0;
        let mut record = |m: &mut [u8], rtype: u8, data: &[u8]| {
            m[at..at + 4].copy_from_slice(&[0xc0, 12, 0, rtype]);
            m[at + 4..at + 6].copy_from_slice(&[0, 1]);
            m[at + 6..at + 10].copy_from_slice(&ttl);
            m[at + 10..at + 12].copy_from_slice(&(data.len() as u16).to_be_bytes());
            m[at + 12..at + 12 + data.len()].copy_from_slice(data);
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
        io.submit(
            Token(S_SEND),
            Op::Send {
                sock: S_SOCK,
                buf: S_BUF,
                off: 0,
                len: at as u32,
                zero_copy: false,
            },
        )
        .unwrap();
    }
}

fn zone_v6(tail: u16) -> Ipv6Addr {
    Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, tail)
}

pub fn config(servers: &[SocketAddr], refresh_ns: u64, family: Family) -> Config {
    Config {
        servers: servers.to_vec(),
        hosts: ZONE.iter().map(|z| z.0.to_string()).collect(),
        family,
        refresh_ns,
        timeout_ns: 50 * MS,
        socks: [Sock(0), Sock(1)],
        rx_bufs: [BufId(0), BufId(1)],
        tx_buf: BufId(2),
        token_base: 1 << 40,
    }
}

/// Run resolver and server until virtual time `until`, with allocation forbidden.
pub fn pump(io: &mut SimIo, r: &mut Resolver, s: &mut Server, out: &mut Completions, until: u64) {
    sealed(|| {
        while io.now_ns() < until {
            io.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if !r.on_completion(io, c) {
                    s.on(io, c.token.0, c.result);
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
    let mut r = Resolver::new(config(servers, refresh, family));
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
