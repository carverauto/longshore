// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A store whose name never resolves: operations must end, not wait for ever.

use osc_dns::Resolver;
use osc_http::client;
use osc_io::alloc_guard::{sealed, Guard};
use osc_io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use osc_sim::{Faults, SimIo};
use osc_store::creds::Credentials;
use osc_store::s3::{Checksum, Config, Endpoint, Fatal, Outcome, SubmitError, Vendor, S3};
use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

const MS: u64 = 1_000_000;
const HEARTBEAT: u64 = 1 << 50;
// Large buffers: user tx 0. Small buffers: one HTTP head, three for name resolution.
const LIMITS: Limits = Limits {
    sockets_max: 3,
    buffers: 1,
    buffer_size: 4096,
    small_buffers: 4,
    small_buffer_size: 2048,
    ops_max: 16,
};

#[test]
fn operations_end_when_the_name_never_resolves() {
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    // Nothing listens at the nameserver's address, so no query is answered.
    let resolver = Resolver::new(osc_dns::Config {
        servers: vec![SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 53), 53)],
        hosts: vec!["bucket.objects.test".into()],
        refresh_ns: 30_000 * MS,
        timeout_ns: 200 * MS,
        socks: [Sock(1), Sock(2)],
        rx_bufs: [LIMITS.small(1), LIMITS.small(2)],
        tx_buf: LIMITS.small(3),
        token_base: 3 << 40,
    });
    let http = client::Config {
        connections_max: 1,
        first_sock: 0,
        first_buf: LIMITS.small(0).0,
        head_max: 2048,
        buffer_size: 4096,
        max_age_ns: 60_000 * MS,
        max_requests: 1000,
        request_timeout_ns: 500 * MS,
        zero_copy_min: 4096,
        token_base: 1 << 40,
        tls_batch: 256 * 1024,
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    };
    let mut s3 = S3::new(Config {
        vendor: Vendor::S3,
        host: "bucket.objects.test".into(),
        port: 80,
        region: "us-test-1".into(),
        credentials: Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
        credentials_file: None,
        requests_max: 2,
        attempts_max: 3,
        backoff_ns: 100 * MS,
        backoff_max_ns: 800 * MS,
        require_checksums: false,
        http,
        tls: None,
        endpoint: Endpoint::Resolved(resolver),
    });
    s3.start(&mut io);
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    s3.store(&mut io, 1, "a", BufId(0), 100, Checksum::None)
        .unwrap();
    s3.erase(&mut io, 2, "b").unwrap();
    assert_eq!(s3.erase(&mut io, 3, "c"), Err(SubmitError::Busy));

    let mut out = Completions::new(16);
    let mut done = Vec::with_capacity(4);
    sealed(|| {
        while done.len() < 2 {
            assert!(
                io.now_ns() < 60_000 * MS,
                "operations were still waiting for an address after a minute"
            );
            io.tick(true, &mut out).expect("simulation deadlock");
            while let Some(c) = out.pop() {
                if c.token.0 == HEARTBEAT {
                    s3.tick(&mut io);
                    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                        .unwrap();
                } else {
                    assert!(s3.on_completion(&mut io, c), "unrouted completion {c:?}");
                }
            }
            while let Some(d) = s3.poll() {
                done.push(d);
            }
        }
    });
    for d in &done {
        assert_eq!(
            (d.outcome, d.attempts),
            (Outcome::Fatal(Fatal::NoAddress), 0),
            "{d:?}"
        );
    }
    // Three attempts of 500 ms each is the time the operations were allowed.
    assert!(
        (1_400 * MS..2_500 * MS).contains(&io.now_ns()),
        "gave up after {} ms",
        io.now_ns() / MS
    );
    // The slots are free again.
    s3.erase(&mut io, 3, "c").unwrap();
}
