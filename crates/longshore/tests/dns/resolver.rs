// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Resolver against a simulated DNS server, with allocation sealed while running.

mod common;
use common::*;

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
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.hosts.insert(0, "missing.example.org".into());
    let mut r = Resolver::new(cfg);
    r.start(&mut io);
    let mut out = Completions::new(8);
    pump(&mut io, &mut r, &mut s, &mut out, 200 * MS);
    assert!(!r.ready() && r.lookup(0).is_none());
    assert_eq!(r.addrs(1), &zone_addrs(0)[..]);
    assert!(r.failures >= 1);
}
