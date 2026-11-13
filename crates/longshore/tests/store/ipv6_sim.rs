// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The store over IPv6: a fixed IPv6 endpoint, a name with only AAAA
//! records, and a name with both families where one cannot be reached.

mod common;
use common::names::*;
use common::*;
use longshore::dns::Family;
use std::net::Ipv6Addr;

const V6: Ipv6Addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x80);
const ADDR_V6: SocketAddr = SocketAddr::new(IpAddr::V6(V6), 80);
const V4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 80);
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
        a: &[],
        aaaa: &[V6],
    };
    let (mut w, ns) = resolved_world(Family::PreferV4, None, ADDR_V6, ns);
    assert_eq!(store(&mut w, &ns, 1, "k"), stored(1, 1));
}

#[test]
fn an_unreachable_preferred_family_costs_one_attempt_then_the_other_is_used() {
    // The name has both families, but nothing listens at its IPv6 address.
    let ns = Nameserver {
        a: &[V4],
        aaaa: &[V6],
    };
    let (mut w, ns) = resolved_world(Family::PreferV6, None, ADDR, ns);
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
