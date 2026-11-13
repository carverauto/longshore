// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The resolver's address families: AAAA records, the preference between
//! families, and moving to the other family when connections fail.

mod common;
use common::*;

fn resolved(family: Family) -> (SimIo, Resolver, Server, Completions) {
    let (mut io, mut r, mut s, mut out) = world_for(1, Faults::default(), &[NS1], NS1, SEC, family);
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    (io, r, s, out)
}

#[test]
fn an_ipv6_only_resolver_caches_aaaa_records_and_asks_for_nothing_else() {
    let (_io, mut r, s, _out) = resolved(Family::V6);
    assert!(r.ready());
    assert_eq!(
        (r.addrs(0), r.addrs(1)),
        (&zone_addrs_v6(0)[..], &zone_addrs_v6(1)[..])
    );
    let seen: Vec<_> = (0..4).map(|_| r.lookup(0).unwrap()).collect();
    assert!(seen.iter().all(IpAddr::is_ipv6));
    assert_eq!(&seen[..2], &seen[2..], "rotation repeats");
    assert_eq!(
        s.queries,
        ZONE.len() as u64 * r.cycles,
        "one query per host"
    );
    assert_eq!(r.failures, 0);
}

#[test]
fn both_families_are_asked_for_and_the_preferred_one_is_handed_out() {
    for (family, want_v6) in [(Family::PreferV6, true), (Family::PreferV4, false)] {
        let (_io, mut r, s, _out) = resolved(family);
        assert_eq!(s.queries, 2 * ZONE.len() as u64 * r.cycles, "{family:?}");
        let want = if want_v6 {
            zone_addrs_v6(0)
        } else {
            zone_addrs(0)
        };
        assert_eq!(r.addrs(0), &want[..], "{family:?}");
        assert_eq!(r.lookup(0).unwrap().is_ipv6(), want_v6, "{family:?}");
        assert_eq!(r.failures, 0);
    }
}

#[test]
fn a_name_without_the_preferred_family_resolves_to_the_other_without_a_failure() {
    let (mut io, mut r, mut s, mut out) =
        world_for(2, Faults::default(), &[NS1], NS1, SEC, Family::PreferV6);
    s.v6 = false;
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC);
    assert!(r.ready());
    assert_eq!(r.addrs(0), &zone_addrs(0)[..]);
    assert_eq!(
        r.failures, 0,
        "an empty answer for one family is not a failure"
    );
}

#[test]
fn a_family_that_disappears_from_the_zone_is_dropped_at_the_next_refresh() {
    let (mut io, mut r, mut s, mut out) = resolved(Family::PreferV6);
    assert!(r.lookup(0).unwrap().is_ipv6());
    s.v6 = false;
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC);
    assert_eq!(r.addrs(0), &zone_addrs(0)[..]);
    assert!(r.lookup(0).unwrap().is_ipv4());
    assert_eq!(r.failures, 0);
    s.v6 = true;
    pump(&mut io, &mut r, &mut s, &mut out, 6 * SEC);
    assert_eq!(r.addrs(0), &zone_addrs_v6(0)[..], "and comes back with it");
}

#[test]
fn a_host_that_moves_from_ipv6_to_ipv4_is_handed_out_by_ipv4() {
    let (mut io, mut r, mut s, mut out) =
        world_for(3, Faults::default(), &[NS1], NS1, SEC, Family::PreferV6);
    s.v4 = false;
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    assert_eq!(r.addrs(0), &zone_addrs_v6(0)[..]);
    // The empty AAAA answer comes before the A answer that makes it final.
    (s.v4, s.v6) = (true, false);
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC / 2);
    assert_eq!(r.addrs(0), &zone_addrs(0)[..]);
    assert!(r.lookup(0).unwrap().is_ipv4());
    assert_eq!(r.failures, 0);
}

#[test]
fn a_zone_that_loses_every_address_keeps_the_last_known_ones() {
    let (mut io, mut r, mut s, mut out) = resolved(Family::PreferV4);
    let cycles = r.cycles;
    (s.v4, s.v6) = (false, false);
    pump(&mut io, &mut r, &mut s, &mut out, 5 * SEC / 2);
    assert!(r.cycles > cycles);
    assert_eq!(
        r.failures,
        (r.cycles - cycles) * ZONE.len() as u64,
        "one failure per host and cycle"
    );
    assert_eq!(r.addrs(0), &zone_addrs(0)[..], "IPv4 is kept");
    let v4 = r.lookup(0).unwrap();
    assert!(v4.is_ipv4());
    r.connect_failed(0, v4);
    assert_eq!(r.family_switches, 1);
    assert_eq!(r.addrs(0), &zone_addrs_v6(0)[..], "and so is IPv6");
}

#[test]
fn a_failed_connection_moves_lookups_to_the_other_family_and_back() {
    let (mut io, mut r, mut s, mut out) = resolved(Family::PreferV6);
    let v6 = r.lookup(0).unwrap();
    assert!(v6.is_ipv6());
    r.connect_failed(0, v6);
    let v4 = r.lookup(0).unwrap();
    assert!(v4.is_ipv4());
    assert_eq!(r.family_switches, 1);
    assert!(r.lookup(1).unwrap().is_ipv6(), "other hosts are unaffected");
    // A late failure of the family already left changes nothing.
    r.connect_failed(0, v6);
    assert!(r.lookup(0).unwrap().is_ipv4());
    assert_eq!(r.family_switches, 1);
    // A refresh does not move lookups back by itself.
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC);
    assert!(r.lookup(0).unwrap().is_ipv4());
    // The family in use failing in turn does.
    r.connect_failed(0, v4);
    assert!(r.lookup(0).unwrap().is_ipv6());
    assert_eq!(r.family_switches, 2);
}

#[test]
fn a_failed_connection_changes_nothing_when_there_is_no_other_family() {
    for family in [Family::V4, Family::V6] {
        let (_io, mut r, _s, _out) = resolved(family);
        let ip = r.lookup(0).unwrap();
        r.connect_failed(0, ip);
        assert_eq!(r.lookup(0).unwrap().is_ipv6(), ip.is_ipv6());
        assert_eq!(r.family_switches, 0);
    }
}
