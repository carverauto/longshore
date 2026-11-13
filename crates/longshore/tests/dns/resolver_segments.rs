// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The resolver's preference for addresses whose connections have large
//! TCP segments.

mod common;
use common::*;
use longshore::io::Segment;

const LARGE: u32 = 8228;
const SMALL: u32 = 1432;
/// What a jumbo-frame path could carry.
const PATH: u32 = 8960;

/// A measurement on a path whose limit is unknown.
fn seg(size: u32) -> Segment {
    Segment {
        size,
        path_limit: 0,
    }
}

/// A measurement on the jumbo-frame path.
fn on_path(size: u32) -> Segment {
    Segment {
        size,
        path_limit: PATH,
    }
}

/// A resolver for host 0's three A records, preferring large segments and
/// keeping at least `keep` addresses, or with no preference.
fn resolved(keep: Option<usize>) -> (SimIo, Resolver, Server, Completions) {
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    let mut s = Server::start(&mut io, NS1);
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.prefer_large_segments = keep;
    let mut r = Resolver::new(cfg);
    r.start(&mut io);
    let mut out = Completions::new(8);
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    (io, r, s, out)
}

/// The distinct addresses the next `n` lookups return, sorted.
fn handed_out(r: &mut Resolver, n: usize) -> Vec<IpAddr> {
    let mut seen: Vec<IpAddr> = (0..n).map(|_| r.lookup(0).unwrap()).collect();
    seen.sort();
    seen.dedup();
    seen
}

#[test]
fn every_address_is_handed_out_until_it_has_been_measured() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    assert_eq!(r.segment_floor(0), 0);
    assert_eq!(handed_out(&mut r, 6), all);
    // One small measurement alone ranks nothing out: it is the largest seen.
    assert_eq!(r.connected(0, all[0], seg(SMALL)), SMALL / 2);
    assert_eq!(handed_out(&mut r, 6), all);
    // A large one does, and the unmeasured address is still tried.
    assert_eq!(r.connected(0, all[1], seg(LARGE)), LARGE / 2);
    assert_eq!(handed_out(&mut r, 6), [all[1], all[2]]);
}

#[test]
fn lookups_converge_on_the_addresses_with_large_segments() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], SMALL), (all[1], LARGE), (all[2], SMALL)] {
        r.connected(0, ip, seg(size));
    }
    assert_eq!(handed_out(&mut r, 9), [all[1]]);
    // Sizes are per address: host 1 is untouched.
    assert_eq!(r.segment_floor(1), 0);
    assert!(r.lookup(1).is_some());
}

#[test]
fn addresses_whose_segments_are_alike_all_stay_in_use() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    // 8228 and 8176 differ by an option's worth of bytes, not by a class.
    for (ip, size) in [(all[0], LARGE), (all[1], 8176), (all[2], LARGE)] {
        r.connected(0, ip, seg(size));
    }
    assert_eq!(handed_out(&mut r, 9), all);
}

#[test]
fn a_minimum_number_of_addresses_is_kept_by_admitting_the_next_largest() {
    let (_io, mut r, _s, _out) = resolved(Some(2));
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], 1200), (all[1], LARGE), (all[2], SMALL)] {
        r.connected(0, ip, seg(size));
    }
    assert_eq!(r.segment_floor(0), SMALL);
    assert_eq!(handed_out(&mut r, 9), [all[1], all[2]]);
    // Asking for more addresses than there are keeps them all.
    let (_io, mut r, _s, _out) = resolved(Some(8));
    for (ip, size) in [(all[0], 1200), (all[1], LARGE), (all[2], SMALL)] {
        r.connected(0, ip, seg(size));
    }
    assert_eq!(handed_out(&mut r, 9), all);
}

#[test]
fn without_the_preference_measurements_change_nothing() {
    let (_io, mut r, _s, _out) = resolved(None);
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], SMALL), (all[1], LARGE), (all[2], SMALL)] {
        assert_eq!(r.connected(0, ip, seg(size)), 0);
    }
    assert_eq!(handed_out(&mut r, 9), all);
}

#[test]
fn a_refresh_keeps_the_sizes_of_addresses_still_in_the_answer() {
    let (mut io, mut r, mut s, mut out) = resolved(Some(1));
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], SMALL), (all[1], LARGE), (all[2], SMALL)] {
        r.connected(0, ip, seg(size));
    }
    let before = r.cycles;
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC);
    assert!(r.cycles > before);
    assert_eq!(handed_out(&mut r, 9), [all[1]]);
    // A measurement for an address the host does not have is ignored.
    let stranger = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
    assert_eq!(r.connected(0, stranger, seg(65_000)), LARGE / 2);
    assert_eq!(handed_out(&mut r, 9), [all[1]]);
}

#[test]
fn a_new_measurement_replaces_the_old_one_for_its_address() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], SMALL), (all[1], LARGE), (all[2], LARGE)] {
        r.connected(0, ip, seg(size));
    }
    assert_eq!(handed_out(&mut r, 9), [all[1], all[2]]);
    // The paths change places.
    r.connected(0, all[0], seg(LARGE));
    r.connected(0, all[1], seg(SMALL));
    assert_eq!(handed_out(&mut r, 9), [all[0], all[2]]);
}

#[test]
fn addresses_all_small_for_the_path_bring_an_early_refresh() {
    let (mut io, mut r, mut s, mut out) = resolved(Some(1));
    let all = zone_addrs(0);
    // Small everywhere, on a path that could carry far more: nothing to
    // prefer, nothing worth replacing, and nothing left to try.
    for ip in &all {
        assert_eq!(r.connected(0, *ip, on_path(SMALL)), 0);
    }
    assert_eq!(r.probes_wanted(0), 0);
    assert_eq!(handed_out(&mut r, 6), all);
    // The refresh interval is a second. Starved, the resolver asks again
    // after one answer timeout (50 ms) instead.
    let before = r.cycles;
    pump(&mut io, &mut r, &mut s, &mut out, 600 * MS);
    assert!(r.cycles >= before + 5, "{} cycles", r.cycles - before);
    assert!(r.early_refreshes >= 5);
    assert_eq!(r.failures, 0);
}

#[test]
fn small_segments_on_a_path_that_carries_no_more_are_not_starvation() {
    let (mut io, mut r, mut s, mut out) = resolved(Some(1));
    for ip in zone_addrs(0) {
        // An Ethernet path: 1432 is what there is to have.
        let segment = Segment {
            size: SMALL,
            path_limit: 1460,
        };
        r.connected(0, ip, segment);
    }
    pump(&mut io, &mut r, &mut s, &mut out, 600 * MS);
    assert_eq!(r.early_refreshes, 0);
}

#[test]
fn unmeasured_addresses_are_probed_first_when_the_measured_ones_are_small() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    r.connected(0, all[0], on_path(SMALL));
    r.connected(0, all[1], on_path(SMALL));
    // No large address is known, so nothing is worth replacing yet, but
    // the one unmeasured address is worth a connection, and lookups go to it.
    assert_eq!((r.segment_floor(0), r.probes_wanted(0)), (0, 1));
    assert_eq!(handed_out(&mut r, 6), [all[2]]);
    // It turns out large: now the small ones are worth replacing.
    assert_eq!(r.connected(0, all[2], on_path(LARGE)), LARGE / 2);
    assert_eq!(r.probes_wanted(0), 0);
    assert_eq!(handed_out(&mut r, 6), [all[2]]);
}

#[test]
fn one_address_small_for_the_path_is_ranked_out_without_seeing_a_large_one() {
    // With the path limit known, "small" needs no larger neighbour to
    // compare with: 1432 is under a quarter of 8960.
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    r.connected(0, all[0], on_path(SMALL));
    assert_eq!(handed_out(&mut r, 6), [all[1], all[2]]);
}
