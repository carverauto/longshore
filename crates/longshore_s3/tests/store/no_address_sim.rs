// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A store whose name never resolves: operations must end, not wait for ever.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_dns::Resolver;
use longshore_http_client::client;
use longshore_s3::creds::Credentials;
use longshore_s3::s3::{Checksum, Config, Done, Endpoint, Fatal, Outcome, SubmitError, Vendor, S3};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

const MS: u64 = 1_000_000;
const HEARTBEAT: u64 = 1 << 50;
// Large buffers: user tx 0. Small buffers: one HTTP head, three for name resolution.
const LIMITS: Limits = Limits::BASE
    .with_sockets_max(3)
    .with_buffers(1)
    .with_buffer_size(4096)
    .with_small_buffers(4)
    .with_small_buffer_size(2048)
    .with_ops_max(16);

#[test]
fn operations_end_when_the_name_never_resolves() {
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    let mut s3 = s3_behind_a_silent_nameserver();
    s3.start(&mut io);
    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    s3.store(&mut io, 1, "a", BufId::new(0), 100, Checksum::None)
        .unwrap();
    s3.erase(&mut io, 2, "b").unwrap();
    assert_eq!(s3.erase(&mut io, 3, "c"), Err(SubmitError::Busy));

    let done = run_until_done(&mut io, &mut s3, 2);
    for d in &done {
        assert_eq!(
            (d.outcome(), d.attempts()),
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

/// A store at a name only a nameserver could resolve. Nothing listens at
/// the nameserver's address, so no query is answered.
#[cfg(test)]
fn s3_behind_a_silent_nameserver() -> S3 {
    let resolver = Resolver::new(
        longshore_dns::Config::new(
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 53)),
                53,
            )],
            vec!["bucket.objects.test".into()],
            [Sock::new(1), Sock::new(2)],
            [LIMITS.small(1), LIMITS.small(2)],
            LIMITS.small(3),
            3 << 40,
        )
        .with_family(longshore_dns::Family::V4)
        .with_prefer_large_segments(None)
        .with_refresh_ns(30_000 * MS)
        .with_timeout_ns(200 * MS),
    )
    .expect("resolver config");
    let http = client::ClientConfig::builder(1, 0, LIMITS.small(0).get(), 4096, 1 << 40)
        .head_max(2048)
        .max_age_ns(60_000 * MS)
        .max_requests(1000)
        .request_timeout_ns(500 * MS)
        .zero_copy_min(4096)
        .tls_batch(256 * 1024)
        .build()
        .unwrap();
    S3::new(
        Config::new(
            Vendor::S3,
            "bucket.objects.test".into(),
            "us-test-1".into(),
            Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
            http,
            Endpoint::Resolved(resolver),
        )
        .with_bucket_in_path(None)
        .with_port(80)
        .with_credentials_file(None)
        .with_requests_max(2)
        .with_attempts_max(3)
        .with_backoff_ns(100 * MS)
        .with_backoff_max_ns(800 * MS)
        .with_require_checksums(false),
    )
}

/// Drives the store with allocation forbidden until `n` operations have
/// ended, failing if that takes a minute of virtual time.
#[cfg(test)]
fn run_until_done(io: &mut SimIo, s3: &mut S3, n: usize) -> Vec<Done> {
    let mut out = Completions::new(16);
    let mut done = Vec::with_capacity(4);
    sealed(|| {
        while done.len() < n {
            assert!(
                io.now_ns() < 60_000 * MS,
                "operations were still waiting for an address after a minute"
            );
            io.tick(true, &mut out).expect("simulation deadlock");
            while let Some(c) = out.pop() {
                if c.token().get() == HEARTBEAT {
                    s3.tick(io);
                    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                        .unwrap();
                } else {
                    assert!(s3.on_completion(io, c), "unrouted completion {c:?}");
                }
            }
            while let Some(d) = s3.poll() {
                done.push(d);
            }
        }
    });
    done
}
