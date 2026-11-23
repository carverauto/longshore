// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The resolver's preference for addresses whose connections have large
//! TCP segments.

#[cfg(test)]
mod common;
use common::*;
use longshore_common::io::Segment;

const LARGE: u32 = 8228;
const SMALL: u32 = 1432;
/// What a jumbo-frame path could carry.
const PATH: u32 = 8960;

/// A measurement on a path whose limit is unknown.
fn seg(size: u32) -> Segment {
    Segment::new(size, 0)
}

/// A measurement on the jumbo-frame path.
fn on_path(size: u32) -> Segment {
    Segment::new(size, PATH)
}

/// A resolver for host 0's three A records, preferring large segments and
/// keeping at least `keep` addresses, or with no preference.
#[cfg(test)]
fn resolved(keep: Option<usize>) -> (SimIo, Resolver, Server, Completions) {
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    let mut s = Server::start(&mut io, NS1);
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.prefer_large_segments = keep;
    let mut r = Resolver::new(cfg).expect("resolver config");
    r.start(&mut io);
    let mut out = Completions::new(8);
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    (io, r, s, out)
}

/// The distinct addresses the next `n` lookups return, sorted.
#[cfg(test)]
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
        let _: u32 = r.connected(0, ip, seg(size));
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
        let _: u32 = r.connected(0, ip, seg(size));
    }
    assert_eq!(handed_out(&mut r, 9), all);
}

#[test]
fn a_minimum_number_of_addresses_is_kept_by_admitting_the_next_largest() {
    let all = zone_addrs(0);
    {
        let (_io, mut r, _s, _out) = resolved(Some(2));
        for (ip, size) in [(all[0], 1200), (all[1], LARGE), (all[2], SMALL)] {
            let _: u32 = r.connected(0, ip, seg(size));
        }
        assert_eq!(r.segment_floor(0), SMALL);
        assert_eq!(handed_out(&mut r, 9), [all[1], all[2]]);
    }
    // Asking for more addresses than there are keeps them all.
    {
        let (_io, mut r, _s, _out) = resolved(Some(8));
        for (ip, size) in [(all[0], 1200), (all[1], LARGE), (all[2], SMALL)] {
            let _: u32 = r.connected(0, ip, seg(size));
        }
        assert_eq!(handed_out(&mut r, 9), all);
    }
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
        let _: u32 = r.connected(0, ip, seg(size));
    }
    let before = r.cycles();
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC);
    assert!(r.cycles() > before);
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
        let _: u32 = r.connected(0, ip, seg(size));
    }
    assert_eq!(handed_out(&mut r, 9), [all[1], all[2]]);
    // The paths change places.
    let _: u32 = r.connected(0, all[0], seg(LARGE));
    let _: u32 = r.connected(0, all[1], seg(SMALL));
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
    let before = r.cycles();
    pump(&mut io, &mut r, &mut s, &mut out, 600 * MS);
    assert!(r.cycles() >= before + 5, "{} cycles", r.cycles() - before);
    assert!(r.early_refreshes() >= 5);
    assert_eq!(r.failures(), 0);
}

#[test]
fn small_segments_on_a_path_that_carries_no_more_are_not_starvation() {
    let (mut io, mut r, mut s, mut out) = resolved(Some(1));
    for ip in zone_addrs(0) {
        // An Ethernet path: 1432 is what there is to have.
        let segment = Segment::new(SMALL, 1460);
        let _: u32 = r.connected(0, ip, segment);
    }
    pump(&mut io, &mut r, &mut s, &mut out, 600 * MS);
    assert_eq!(r.early_refreshes(), 0);
}

#[test]
fn unmeasured_addresses_are_probed_first_when_the_measured_ones_are_small() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    let _: u32 = r.connected(0, all[0], on_path(SMALL));
    let _: u32 = r.connected(0, all[1], on_path(SMALL));
    // No large address is known, so nothing is worth replacing yet, but
    // the one unmeasured address is worth a connection, and lookups go to it.
    assert_eq!((r.segment_floor(0), r.probes_wanted(0)), (0, 1));
    assert_eq!(handed_out(&mut r, 6), [all[2]]);
    // It turns out large: now the small ones are worth replacing.
    assert_eq!(r.connected(0, all[2], on_path(LARGE)), LARGE / 2);
    assert_eq!(r.probes_wanted(0), 0);
    assert_eq!(handed_out(&mut r, 6), [all[2]]);
}

/// "Large" is at least half the largest size seen, rounded up, and an
/// address exactly there is in.
#[test]
fn the_floor_is_half_the_largest_rounded_up_and_includes_its_own_value() {
    for (largest, half) in [(8228, 4114), (8229, 4115), (3, 2), (1, 1)] {
        let (_io, mut r, _s, _out) = resolved(Some(1));
        let all = zone_addrs(0);
        let _: u32 = r.connected(0, all[0], seg(largest));
        let _: u32 = r.connected(0, all[1], seg(half));
        assert_eq!(r.connected(0, all[2], seg(half - 1)), half, "{largest}");
        assert_eq!(r.segment_floor(0), half, "{largest}");
        // An address measured at zero is one not measured, so with a floor
        // of 1 the third address is still handed out.
        let want = if half == 1 {
            all.clone()
        } else {
            vec![all[0], all[1]]
        };
        assert_eq!(handed_out(&mut r, 9), want, "{largest}");
    }
}

/// With the path's limit known, "large" is also at least a quarter of it,
/// and an address exactly there is in.
#[test]
fn a_quarter_of_the_path_limit_is_large_and_one_byte_less_is_not() {
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    assert_eq!(r.connected(0, all[0], on_path(PATH / 4)), PATH / 4);
    assert_eq!(r.connected(0, all[1], on_path(PATH / 4 - 1)), PATH / 4);
    // The third address is unmeasured and is tried with the large one.
    assert_eq!(handed_out(&mut r, 9), [all[0], all[2]]);
    assert_eq!(r.probes_wanted(0), 0, "a large address is already known");
}

#[test]
fn starvation_begins_one_byte_under_a_quarter_of_the_path_limit() {
    for (size, starved) in [(PATH / 4, false), (PATH / 4 - 1, true)] {
        let (mut io, mut r, mut s, mut out) = resolved(Some(1));
        for ip in zone_addrs(0) {
            let _: u32 = r.connected(0, ip, on_path(size));
        }
        pump(&mut io, &mut r, &mut s, &mut out, 600 * MS);
        assert_eq!(r.early_refreshes() > 0, starved, "size {size}");
        assert_eq!(handed_out(&mut r, 6), zone_addrs(0), "size {size}");
    }
}

/// Keeping no addresses is not a thing: zero is taken as one.
#[test]
fn keeping_zero_addresses_keeps_one() {
    let (_io, mut r, _s, _out) = resolved(Some(0));
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], SMALL), (all[1], LARGE), (all[2], SMALL)] {
        let _: u32 = r.connected(0, ip, seg(size));
    }
    assert_eq!(r.segment_floor(0), LARGE / 2);
    assert_eq!(handed_out(&mut r, 9), [all[1]]);
}

/// The preference can be turned on and off while running; measurements
/// made in the meantime count once it is on.
#[test]
fn the_preference_can_be_switched_while_running() {
    let (_io, mut r, _s, _out) = resolved(None);
    let all = zone_addrs(0);
    for (ip, size) in [(all[0], SMALL), (all[1], LARGE), (all[2], SMALL)] {
        let _: u32 = r.connected(0, ip, seg(size));
    }
    assert_eq!((r.segment_floor(0), r.probes_wanted(0)), (0, 0));
    assert_eq!(handed_out(&mut r, 9), all);
    r.set_prefer_large_segments(Some(1));
    assert_eq!(r.segment_floor(0), LARGE / 2);
    assert_eq!(handed_out(&mut r, 9), [all[1]]);
    r.set_prefer_large_segments(Some(2));
    assert_eq!(r.segment_floor(0), SMALL);
    assert_eq!(
        handed_out(&mut r, 9),
        all,
        "both small ones are next largest"
    );
    r.set_prefer_large_segments(None);
    assert_eq!(r.segment_floor(0), 0);
    assert_eq!(handed_out(&mut r, 9), all);
}

#[test]
fn one_address_small_for_the_path_is_ranked_out_without_seeing_a_large_one() {
    // With the path limit known, "small" needs no larger neighbour to
    // compare with: 1432 is under a quarter of 8960.
    let (_io, mut r, _s, _out) = resolved(Some(1));
    let all = zone_addrs(0);
    let _: u32 = r.connected(0, all[0], on_path(SMALL));
    assert_eq!(handed_out(&mut r, 6), [all[1], all[2]]);
}
