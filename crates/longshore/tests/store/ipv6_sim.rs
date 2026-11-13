// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The store over IPv6: a fixed IPv6 endpoint, a name with only AAAA
//! records, and a name with both families where one cannot be reached.

mod common;
use common::*;
use longshore::dns::{Family, Resolver};
use std::net::Ipv6Addr;

const V6: Ipv6Addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x80);
const ADDR_V6: SocketAddr = SocketAddr::new(IpAddr::V6(V6), 80);
const NS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 53)), 53);

// The shared layout, plus two sockets and three small buffers for the
// resolver and one of each for the nameserver.
const WIDE: Limits = Limits {
    sockets_max: LIMITS.sockets_max + 3,
    small_buffers: LIMITS.small_buffers + 4,
    ops_max: 64,
    ..LIMITS
};
const NS_SOCK: Sock = Sock(LIMITS.sockets_max + 2);
/// The nameserver's buffer: the small buffer after the resolver's three.
fn ns_buf() -> BufId {
    WIDE.small(CONNS + 3)
}
const NS_RECV: u64 = 5 << 40;
const NS_SEND: u64 = NS_RECV + 1;

/// A nameserver that knows one name, with at most one address per family.
struct Nameserver {
    a: Option<Ipv4Addr>,
    aaaa: Option<Ipv6Addr>,
}

impl Nameserver {
    fn recv(io: &mut SimIo) {
        io.submit(
            Token(NS_RECV),
            Op::Recv {
                sock: NS_SOCK,
                buf: ns_buf(),
                off: 0,
                len: 512,
            },
        )
        .unwrap();
    }

    /// Answer the query in the buffer in place. False if the token is not ours.
    fn on(&self, io: &mut SimIo, token: u64, result: i32) -> bool {
        match token {
            NS_SEND => Self::recv(io),
            NS_RECV => {
                let n = result as usize;
                let m = io.buf(ns_buf());
                let rtype = m[n - 3];
                let aaaa = rtype == 28;
                let mut data = [0u8; 16];
                let len = match (aaaa, self.a, self.aaaa) {
                    (false, Some(a), _) => {
                        data[..4].copy_from_slice(&a.octets());
                        4
                    }
                    (true, _, Some(a)) => {
                        data.copy_from_slice(&a.octets());
                        16
                    }
                    _ => 0,
                };
                (m[2], m[3]) = (0x81, 0x80);
                let mut end = n;
                if len > 0 {
                    m[7] = 1;
                    m[n..n + 12]
                        .copy_from_slice(&[0xc0, 12, 0, rtype, 0, 1, 0, 0, 0, 60, 0, len as u8]);
                    m[n + 12..n + 12 + len].copy_from_slice(&data[..len]);
                    end = n + 12 + len;
                }
                io.submit(
                    Token(NS_SEND),
                    Op::Send {
                        sock: NS_SOCK,
                        buf: ns_buf(),
                        off: 0,
                        len: end as u32,
                        zero_copy: false,
                    },
                )
                .unwrap();
            }
            _ => return false,
        }
        true
    }
}

/// A store whose endpoint is a name, an object server listening at
/// `listen`, and the nameserver.
fn resolved_world(family: Family, listen: SocketAddr, ns: Nameserver) -> (World, Nameserver) {
    let mut io = SimIo::new(WIDE, 7, Faults::default());
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 0,
        head_max: 2048,
        buffer_size: BUF,
        objects_max: 8,
        token_base: 2 << 40,
        checksum: None,
    });
    server.start(&mut io, listen).unwrap();
    io.bind_udp(NS_SOCK, NS);
    Nameserver::recv(&mut io);
    let mut cfg = s3_config(3, false);
    cfg.endpoint = Endpoint::Resolved(Resolver::new(longshore::dns::Config {
        servers: vec![NS],
        hosts: vec![cfg.host.clone()],
        family,
        refresh_ns: 30_000 * MS,
        timeout_ns: 200 * MS,
        socks: [Sock(LIMITS.sockets_max), Sock(LIMITS.sockets_max + 1)],
        rx_bufs: [WIDE.small(CONNS), WIDE.small(CONNS + 1)],
        tx_buf: WIDE.small(CONNS + 2),
        token_base: 3 << 40,
    }));
    let mut s3 = S3::new(cfg);
    s3.start(&mut io);
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    let world = World {
        io,
        s3,
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(8),
    };
    (world, ns)
}

/// Store one object and return how the operation ended.
fn store(w: &mut World, ns: &Nameserver, token: u64, key: &str) -> Done {
    w.fill_tx(100, token as u8);
    w.s3.store(&mut w.io, token, key, TX, 100, Checksum::None)
        .unwrap();
    let World {
        io,
        s3,
        server,
        out,
        ..
    } = w;
    sealed(|| loop {
        io.tick(true, out).expect("simulation deadlock");
        while let Some(c) = out.pop() {
            if c.token.0 == HEARTBEAT {
                s3.tick(io);
                io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                    .unwrap();
            } else if !s3.on_completion(io, c) && !ns.on(io, c.token.0, c.result) {
                assert!(server.on_completion(io, c), "unrouted completion {c:?}");
            }
        }
        if let Some(d) = s3.poll() {
            return d;
        }
    })
}

fn stored(token: u64, attempts: u32) -> Done {
    Done {
        token,
        outcome: Outcome::Stored,
        attempts,
    }
}

#[test]
fn operations_complete_over_a_fixed_ipv6_endpoint() {
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 0,
        head_max: 2048,
        buffer_size: BUF,
        objects_max: 8,
        token_base: 2 << 40,
        checksum: None,
    });
    server.start(&mut io, ADDR_V6).unwrap();
    let mut cfg = s3_config(3, false);
    cfg.endpoint = Endpoint::Fixed(ADDR_V6);
    let mut s3 = S3::new(cfg);
    s3.start(&mut io);
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    let mut w = World {
        io,
        s3,
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(8),
    };
    w.fill_tx(100, 0);
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::None)
        .unwrap();
    assert_eq!(w.last(), stored(1, 1));
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    let d = w.last();
    assert_eq!((d.token, d.attempts), (2, 1));
    assert!(
        matches!(d.outcome, Outcome::Fetched { len: 100, .. }),
        "{d:?}"
    );
}

#[test]
fn a_name_with_only_aaaa_records_is_reached_over_ipv6() {
    // The preference is for IPv4, which the name does not have.
    let ns = Nameserver {
        a: None,
        aaaa: Some(V6),
    };
    let (mut w, ns) = resolved_world(Family::PreferV4, ADDR_V6, ns);
    assert_eq!(store(&mut w, &ns, 1, "k"), stored(1, 1));
}

#[test]
fn an_unreachable_preferred_family_costs_one_attempt_then_the_other_is_used() {
    // The name has both families, but nothing listens at its IPv6 address.
    let ns = Nameserver {
        a: Some(Ipv4Addr::new(192, 0, 2, 80)),
        aaaa: Some(V6),
    };
    let (mut w, ns) = resolved_world(Family::PreferV6, ADDR, ns);
    assert_eq!(
        store(&mut w, &ns, 1, "k"),
        stored(1, 2),
        "one refused connection over IPv6, then IPv4"
    );
    assert_eq!(
        store(&mut w, &ns, 2, "k2"),
        stored(2, 1),
        "later operations go straight to the family that works"
    );
}
