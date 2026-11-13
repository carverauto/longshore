// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The store with a preference for large TCP segments, against a name
//! whose addresses differ in the segment size their connections get.

mod common;
use common::names::*;
use common::*;
use longshore::dns::Family;

const LARGE: u32 = 8228;
const SMALL: u32 = 1432;
const FRONTS: [Ipv4Addr; 3] = [
    Ipv4Addr::new(192, 0, 2, 81),
    Ipv4Addr::new(192, 0, 2, 82),
    Ipv4Addr::new(192, 0, 2, 83),
];

fn front(i: usize) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(FRONTS[i]), 80)
}

/// A store behind three front-end addresses that all reach one server.
/// Connections to the first two get small segments, and to the third large.
fn world_of_fronts(prefer: Option<usize>, sizes: [u32; 3]) -> (World, Nameserver) {
    let ns = Nameserver {
        a: &FRONTS,
        aaaa: &[],
    };
    let (mut w, ns) = resolved_world(Family::V4, prefer, ADDR, ns);
    for (i, size) in sizes.into_iter().enumerate() {
        w.io.route(front(i), ADDR, size);
    }
    (w, ns)
}

/// Store three objects at once, which opens a connection to each address.
fn store_three_at_once(w: &mut World, ns: &Nameserver) {
    w.fill_tx(100, 0);
    for (token, key) in [(1, "a"), (2, "b"), (3, "c")] {
        w.s3.store(&mut w.io, token, key, TX, 100, Checksum::None)
            .unwrap();
    }
    for _ in 0..3 {
        let d = next_done(w, ns);
        assert_eq!((d.outcome, d.attempts), (Outcome::Stored, 1), "{d:?}");
    }
}

/// Store `n` objects one after another. Every one must succeed at once.
fn store_many(w: &mut World, ns: &Nameserver, first: u64, n: u64) {
    for token in first..first + n {
        let d = store(w, ns, token, "k");
        assert_eq!((d.outcome, d.attempts), (Outcome::Stored, 1), "{d:?}");
    }
}

#[test]
fn connections_move_to_the_address_with_large_segments() {
    let (mut w, ns) = world_of_fronts(Some(1), [SMALL, SMALL, LARGE]);
    store_three_at_once(&mut w, &ns);
    assert_eq!(
        [0, 1, 2].map(|i| w.io.route_connects(front(i))),
        [1, 1, 1],
        "one connection to each address"
    );
    assert_eq!(
        w.s3.small_segment_retired, 2,
        "the two connections with small segments are not reused"
    );
    // Three more at once: the retired connections are replaced, and the
    // new ones go to the large-segment address.
    store_three_at_once(&mut w, &ns);
    assert_eq!([0, 1, 2].map(|i| w.io.route_connects(front(i))), [1, 1, 3]);
    // From here on nothing new is opened.
    store_many(&mut w, &ns, 100, 12);
    store_three_at_once(&mut w, &ns);
    assert_eq!([0, 1, 2].map(|i| w.io.route_connects(front(i))), [1, 1, 3]);
    assert_eq!((w.s3.connects(), w.s3.small_segment_retired), (5, 2));
}

#[test]
fn a_client_with_one_connection_learns_one_address_and_keeps_it() {
    // Nothing is known about the other addresses, so the small-segment
    // connection is the best seen and stays in use.
    let (mut w, ns) = world_of_fronts(Some(1), [SMALL, SMALL, LARGE]);
    store_many(&mut w, &ns, 1, 12);
    assert_eq!((w.s3.connects(), w.s3.small_segment_retired), (1, 0));
}

#[test]
fn equal_segment_sizes_leave_every_address_in_use() {
    let (mut w, ns) = world_of_fronts(Some(1), [LARGE, LARGE, LARGE]);
    store_three_at_once(&mut w, &ns);
    store_three_at_once(&mut w, &ns);
    assert_eq!(w.s3.small_segment_retired, 0);
    assert_eq!(w.s3.connects(), 3, "three connections, reused");
}

#[test]
fn without_the_preference_a_small_segment_connection_is_kept() {
    let (mut w, ns) = world_of_fronts(None, [SMALL, SMALL, LARGE]);
    store_three_at_once(&mut w, &ns);
    store_three_at_once(&mut w, &ns);
    assert_eq!(w.s3.small_segment_retired, 0);
    assert_eq!(w.s3.connects(), 3, "three connections, reused");
}

#[test]
fn an_unmeasured_backend_changes_nothing() {
    // Size 0 is what a backend reports when it does not measure.
    let (mut w, ns) = world_of_fronts(Some(1), [0, 0, 0]);
    store_three_at_once(&mut w, &ns);
    store_three_at_once(&mut w, &ns);
    assert_eq!(w.s3.small_segment_retired, 0);
    assert_eq!(w.s3.connects(), 3);
}
