// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Resolver against a simulated DNS server, with allocation sealed while running.

use longshore::dns::{Config, Resolver};
use longshore::io::alloc_guard::{sealed, Guard};
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};
use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

const LIMITS: Limits = Limits {
    sockets_max: 4,
    buffers: 4,
    buffer_size: 1024,
    small_buffers: 0,
    small_buffer_size: 0,
    ops_max: 8,
};
const NS1: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 53), 53);
const NS2: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 54), 53);
const MS: u64 = 1_000_000;
const SEC: u64 = 1000 * MS;

const ZONE: [(&str, &[[u8; 4]]); 2] = [
    (
        "s3.example.com",
        &[[10, 1, 0, 1], [10, 1, 0, 2], [10, 1, 0, 3]],
    ),
    ("blob.example.net", &[[10, 2, 0, 1]]),
];

/// Simulated nameserver on socket 3 / buffer 3. Answers in place, no allocation.
struct Server {
    answering: bool,
    ttl: u32,
    queries: u64,
}

const S_SOCK: Sock = Sock(3);
const S_BUF: BufId = BufId(3);
const S_RECV: u64 = 1;
const S_SEND: u64 = 2;

impl Server {
    fn start(io: &mut SimIo, addr: SocketAddrV4) -> Server {
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
            ttl: 300,
            queries: 0,
        }
    }

    fn on(&mut self, io: &mut SimIo, token: u64, result: i32) {
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
        let ttl = self.ttl;
        let m = io.buf(S_BUF);
        // The question name runs from byte 12 to 5 bytes before the end.
        let addrs = ZONE.iter().find(|(name, _)| {
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
        m[2] = 0x81;
        m[3] = if addrs.is_some() { 0x80 } else { 0x83 };
        let recs = addrs.map(|a| a.1).unwrap_or(&[]);
        m[7] = recs.len() as u8;
        let mut at = n;
        for a in recs {
            m[at..at + 12].copy_from_slice(&[
                0xc0,
                12,
                0,
                1,
                0,
                1,
                (ttl >> 24) as u8,
                (ttl >> 16) as u8,
                (ttl >> 8) as u8,
                ttl as u8,
                0,
                4,
            ]);
            m[at + 12..at + 16].copy_from_slice(a);
            at += 16;
        }
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

fn config(servers: &[SocketAddrV4], refresh_ns: u64) -> Config {
    Config {
        servers: servers.to_vec(),
        hosts: ZONE.iter().map(|z| z.0.to_string()).collect(),
        refresh_ns,
        timeout_ns: 50 * MS,
        socks: [Sock(0), Sock(1)],
        rx_bufs: [BufId(0), BufId(1)],
        tx_buf: BufId(2),
        token_base: 1 << 40,
    }
}

/// Run resolver and server until virtual time `until`, with allocation forbidden.
fn pump(io: &mut SimIo, r: &mut Resolver, s: &mut Server, out: &mut Completions, until: u64) {
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

fn world(
    seed: u64,
    faults: Faults,
    servers: &[SocketAddrV4],
    bind: SocketAddrV4,
    refresh: u64,
) -> (SimIo, Resolver, Server, Completions) {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let s = Server::start(&mut io, bind);
    let mut r = Resolver::new(config(servers, refresh));
    r.start(&mut io);
    (io, r, s, Completions::new(8))
}

fn zone_addrs(i: usize) -> Vec<Ipv4Addr> {
    ZONE[i].1.iter().map(|a| Ipv4Addr::from(*a)).collect()
}

#[test]
fn resolves_every_host_at_startup_and_rotates_addresses() {
    let (mut io, mut r, mut s, mut out) = world(1, Faults::default(), &[NS1], NS1, SEC);
    assert!(!r.ready() && r.lookup(0).is_none());
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    assert!(r.ready());
    assert_eq!(
        (r.addrs(0), r.addrs(1)),
        (&zone_addrs(0)[..], &zone_addrs(1)[..])
    );
    assert_eq!(r.host_index("S3.EXAMPLE.COM"), Some(0));
    assert_eq!(
        r.host_index("s3.example.com."),
        Some(0),
        "trailing dot is the same name"
    );
    let seen: Vec<_> = (0..6).map(|_| r.lookup(0).unwrap()).collect();
    assert_eq!(&seen[..3], &seen[3..], "rotation repeats");
    let mut distinct = seen[..3].to_vec();
    distinct.sort();
    assert_eq!(distinct, zone_addrs(0), "rotation covers every address");
    assert_eq!(r.failures, 0);
}

#[test]
fn refresh_runs_on_the_configured_interval_whatever_the_ttl() {
    let cycles = |ttl: u32| {
        let (mut io, mut r, mut s, mut out) = world(2, Faults::default(), &[NS1], NS1, SEC);
        s.ttl = ttl;
        pump(&mut io, &mut r, &mut s, &mut out, 10 * SEC + 500 * MS);
        (r.cycles, s.queries)
    };
    let short = cycles(1);
    assert_eq!(
        short,
        cycles(86_400),
        "TTL must not change the refresh cadence"
    );
    assert!(
        (10..=11).contains(&short.0),
        "expected one cycle per second over 10.5 s, got {}",
        short.0
    );
    assert_eq!(
        short.1,
        short.0 * ZONE.len() as u64,
        "one query per host per cycle"
    );
}

#[test]
fn a_failed_refresh_keeps_the_previous_answer() {
    let (mut io, mut r, mut s, mut out) = world(3, Faults::default(), &[NS1], NS1, SEC);
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    assert!(r.ready());
    s.answering = false;
    pump(&mut io, &mut r, &mut s, &mut out, 5 * SEC);
    assert!(
        r.failures >= 4,
        "timeouts should be counted, got {}",
        r.failures
    );
    assert_eq!(
        (r.addrs(0), r.addrs(1)),
        (&zone_addrs(0)[..], &zone_addrs(1)[..])
    );
    assert!(r.lookup(0).is_some());
    s.answering = true;
    let before = r.cycles;
    pump(&mut io, &mut r, &mut s, &mut out, 8 * SEC);
    assert!(
        r.cycles > before,
        "refresh resumes once the server answers again"
    );
}

#[test]
fn falls_back_to_the_second_nameserver() {
    // Nothing is bound at NS1; only NS2 answers.
    let (mut io, mut r, mut s, mut out) = world(4, Faults::default(), &[NS1, NS2], NS2, SEC);
    pump(&mut io, &mut r, &mut s, &mut out, SEC);
    assert!(r.ready());
    assert!(
        r.failures >= 1,
        "the dead server should cost at least one timeout"
    );
}

#[test]
fn survives_lost_and_delayed_datagrams_without_caching_a_wrong_address() {
    let faults = Faults {
        udp_drop: 400_000,
        bit_flip: 50_000,
        delay: 300_000,
        ..Faults::default()
    };
    for seed in 0..100 {
        let (mut io, mut r, mut s, mut out) = world(seed, faults, &[NS1], NS1, 200 * MS);
        pump(&mut io, &mut r, &mut s, &mut out, 20 * SEC);
        assert!(
            r.ready(),
            "seed {seed}: not resolved after 20 s with 40% loss"
        );
        for i in 0..ZONE.len() {
            assert_eq!(
                r.addrs(i),
                &zone_addrs(i)[..],
                "seed {seed}: wrong addresses cached for host {i}"
            );
        }
    }
}

#[test]
fn unknown_host_counts_as_failure_and_does_not_block_the_others() {
    let mut io = SimIo::new(LIMITS, 5, Faults::default());
    let mut s = Server::start(&mut io, NS1);
    let mut cfg = config(&[NS1], SEC);
    cfg.hosts.insert(0, "missing.example.org".into());
    let mut r = Resolver::new(cfg);
    r.start(&mut io);
    let mut out = Completions::new(8);
    pump(&mut io, &mut r, &mut s, &mut out, 200 * MS);
    assert!(!r.ready() && r.lookup(0).is_none());
    assert_eq!(r.addrs(1), &zone_addrs(0)[..]);
    assert!(r.failures >= 1);
}
