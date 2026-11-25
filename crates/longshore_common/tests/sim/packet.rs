// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The simulated packet link: each packet once and in order without
//! faults, each fault alone at its rate, what happens when slots and
//! queues run out, the same trace for the same seed, and the invariants
//! of its design after every one of many random calls. Everything after
//! a link is built runs under the allocation guard.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::sim::{End, PacketError, PacketFaults, PacketLink, PacketSizes};

#[path = "packet/random.rs"]
mod random;

#[global_allocator]
static ALLOC: Guard = Guard;

const MAX: usize = 256;
const LATENCY: u64 = 100_000;
const HOLD: u64 = 1_000_000;

/// Packet number `id` from `from`: its number, its sender, and a pattern
/// of a length that the number chooses. Its length.
fn fill(into: &mut [u8; MAX], from: End, id: u64) -> usize {
    let len = 9 + (id as usize * 7) % (MAX - 9);
    into[..8].copy_from_slice(&id.to_le_bytes());
    into[8] = u8::from(from == End::B);
    for (at, b) in into[9..len].iter_mut().enumerate() {
        *b = (at as u8).wrapping_mul(13) ^ (id as u8);
    }
    len
}

/// The number of the packet in `got`, which must be whole and from the
/// end other than `at`.
fn number(got: &[u8], at: End) -> u64 {
    let mut id = [0; 8];
    id.copy_from_slice(&got[..8]);
    let id = u64::from_le_bytes(id);
    let mut same = [0; MAX];
    let len = fill(&mut same, at.other(), id);
    assert!(
        got == &same[..len],
        "packet {id} came changed, or to its sender"
    );
    id
}

/// Send `count` packets from A, a turn after each, then turns until
/// nothing is on its way; the numbers B received with the time of each,
/// and the link.
fn stream(seed: u64, faults: PacketFaults, count: u64) -> (Vec<(u64, u64)>, PacketLink) {
    let mut link = PacketLink::new(seed, faults, PacketSizes::new(64, MAX as u32));
    let mut got = Vec::with_capacity(2 * count as usize);
    let (mut packet, mut into) = ([0; MAX], [0; MAX]);
    let mut now = 0;
    let mut drain = |on: &mut PacketLink, at: u64, seen: &mut Vec<(u64, u64)>| {
        sealed(|| on.turn(at));
        while let Some(len) = sealed(|| on.recv(End::B, &mut into)) {
            let id = number(&into[..len], End::B);
            assert!(seen.len() < seen.capacity());
            seen.push((id, at));
        }
    };
    for id in 0..count {
        let len = fill(&mut packet, End::A, id);
        assert_eq!(sealed(|| link.send(End::A, &packet[..len])), Ok(()));
        drain(&mut link, now, &mut got);
        now += 10_000;
    }
    while let Some(due) = link.next_due(now) {
        now = due;
        drain(&mut link, now, &mut got);
    }
    (got, link)
}

#[test]
fn without_faults_every_packet_arrives_once_in_order_a_latency_later() {
    let (got, link) = stream(1, PacketFaults::NONE, 500);
    assert_eq!(got.len(), 500);
    for (at, (id, when)) in got.iter().enumerate() {
        assert_eq!(*id, at as u64);
        // Sent in the turn at 10,000 ns times its number.
        assert!(*when >= *id * 10_000 + LATENCY, "packet {id} at {when}");
        assert!(
            *when < *id * 10_000 + LATENCY + 10_000,
            "packet {id} at {when}"
        );
    }
    let counts = link.counts();
    assert_eq!((counts.sent(), counts.received()), (500, 500));
    assert_eq!(counts.places(), [64, 0, 0, 0]);
}

#[test]
fn loss_takes_its_share_and_leaves_the_rest_in_order() {
    let (got, link) = stream(2, PacketFaults::NONE.with_loss(100_000), 20_000);
    let lost = link.counts().lost();
    assert!((1700..2300).contains(&lost), "{lost} of 20000 lost");
    assert_eq!(got.len() as u64 + lost, 20_000);
    assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "out of order");
}

#[test]
fn a_duplicate_comes_next_to_its_original() {
    let (got, link) = stream(3, PacketFaults::NONE.with_duplicate(100_000), 20_000);
    let twice = link.counts().duplicated();
    assert!((1700..2300).contains(&twice), "{twice} of 20000 twice");
    assert_eq!(got.len() as u64, 20_000 + twice);
    // Never three, never apart, never out of order.
    assert!(got.windows(2).all(|w| w[0].0 <= w[1].0));
    assert!(got.windows(3).all(|w| w[0].0 != w[2].0));
}

#[test]
fn a_reordered_packet_is_overtaken_and_still_arrives_once() {
    let (got, link) = stream(4, PacketFaults::NONE.with_reorder(100_000), 20_000);
    let held = link.counts().held_back();
    assert!((1700..2300).contains(&held), "{held} of 20000 held back");
    assert_eq!(got.len(), 20_000);
    let overtaken = got.windows(2).filter(|w| w[0].0 > w[1].0).count();
    assert!(overtaken > 1000, "{overtaken} out of order");
    let mut ids: Vec<u64> = got.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    assert!(ids.iter().enumerate().all(|(at, id)| *id == at as u64));
    for (id, when) in &got {
        let sent = *id * 10_000;
        assert!(*when >= sent + LATENCY && *when < sent + LATENCY + HOLD + 10_000);
    }
}

#[test]
fn a_delay_is_within_its_bound() {
    let faults = PacketFaults::NONE.with_delay(500_000, 300_000);
    let (got, link) = stream(5, faults, 20_000);
    let late = link.counts().delayed();
    assert!((9000..11_000).contains(&late), "{late} of 20000 delayed");
    assert_eq!(got.len(), 20_000);
    for (id, when) in &got {
        let sent = *id * 10_000;
        assert!(*when >= sent + LATENCY && *when <= sent + LATENCY + 300_000 + 10_000);
    }
}

#[test]
fn a_full_link_refuses_and_a_full_receive_queue_drops() {
    let sizes = PacketSizes::new(8, MAX as u32).with_queue(3);
    let mut link = PacketLink::new(6, PacketFaults::NONE, sizes);
    let (mut packet, mut into) = ([0; MAX], [0; MAX]);
    sealed(|| {
        assert_eq!(link.send(End::A, &[0; MAX + 1]), Err(PacketError::TooLong));
        // The transmit queue holds three.
        for id in 0..3 {
            let len = fill(&mut packet, End::A, id);
            assert_eq!(link.send(End::A, &packet[..len]), Ok(()));
        }
        assert_eq!(link.send(End::A, &packet[..9]), Err(PacketError::Full));
        assert_eq!(link.counts().places(), [5, 3, 0, 0]);
        link.turn(0);
        assert_eq!(link.counts().places(), [5, 0, 3, 0]);
        // Three more, and the slots: eight in all, with B's two.
        for id in 3..6 {
            let len = fill(&mut packet, End::A, id);
            assert_eq!(link.send(End::A, &packet[..len]), Ok(()));
        }
        for id in 0..2 {
            let len = fill(&mut packet, End::B, id);
            assert_eq!(link.send(End::B, &packet[..len]), Ok(()));
        }
        assert_eq!(link.send(End::B, &packet[..9]), Err(PacketError::Full));
        assert_eq!(link.counts().places(), [0, 5, 3, 0]);
        // Six arrive at B, whose receive queue holds three.
        link.turn(LATENCY);
        link.turn(2 * LATENCY);
        let counts = link.counts();
        assert_eq!((counts.overrun(), counts.places()), (3, [3, 0, 0, 5]));
        for id in 0..3 {
            let len = link.recv(End::B, &mut into).unwrap();
            assert_eq!(number(&into[..len], End::B), id);
        }
        assert_eq!(link.recv(End::B, &mut into), None);
        for id in 0..2 {
            let len = link.recv(End::A, &mut into).unwrap();
            assert_eq!(number(&into[..len], End::A), id);
        }
        assert_eq!(link.counts().places(), [8, 0, 0, 0]);
        assert_eq!(link.next_due(2 * LATENCY), None);
    });
}

#[test]
fn the_same_seed_gives_the_same_packets_at_the_same_times() {
    let faults = PacketFaults::NONE
        .with_loss(50_000)
        .with_duplicate(50_000)
        .with_reorder(50_000)
        .with_delay(200_000, 400_000);
    let mut traces = Vec::new();
    for seed in 0..20 {
        let (once, first) = stream(seed, faults, 2000);
        let (again, second) = stream(seed, faults, 2000);
        assert!(once == again, "seed {seed}");
        assert_eq!(first.trace_hash(), second.trace_hash(), "seed {seed}");
        assert_eq!(first.counts(), second.counts(), "seed {seed}");
        traces.push(first.trace_hash());
    }
    traces.sort_unstable();
    traces.dedup();
    assert_eq!(traces.len(), 20, "two seeds gave one trace");
}

#[test]
fn the_trace_tells_which_packet_was_lost() {
    // Two packets of one length in one turn: a run that loses the first
    // and one that loses the second must not share a trace.
    let mut traces = Vec::new();
    for seed in 0..400 {
        let faults = PacketFaults::NONE.with_loss(500_000);
        let mut link = PacketLink::new(seed, faults, PacketSizes::new(4, MAX as u32));
        let mut into = [0; MAX];
        sealed(|| {
            assert_eq!(link.send(End::A, &[1; 40]), Ok(()));
            assert_eq!(link.send(End::A, &[2; 40]), Ok(()));
            link.turn(0);
            link.turn(LATENCY);
        });
        let mut got = [0u8; 2];
        while let Some(len) = sealed(|| link.recv(End::B, &mut into)) {
            assert_eq!(len, 40);
            got[usize::from(into[0] - 1)] = 1;
        }
        traces.push((got, link.trace_hash()));
    }
    let of = |which: [u8; 2]| {
        let found = traces.iter().find(|(got, _)| *got == which);
        found.map(|(_, trace)| *trace)
    };
    let (first, second) = (of([1, 0]), of([0, 1]));
    assert!(first.is_some() && second.is_some(), "neither case came up");
    assert_ne!(first, second);
}

#[test]
fn what_is_due_next_is_never_in_the_past() {
    let mut link = PacketLink::new(8, PacketFaults::NONE, PacketSizes::new(4, MAX as u32));
    sealed(|| {
        assert_eq!(link.send(End::A, &[7; 20]), Ok(()));
        assert_eq!(link.next_due(50), Some(50));
        link.turn(50);
        assert_eq!(link.next_due(60), Some(50 + LATENCY));
        // The caller's clock passes the time with no turn between.
        let late = 50 + LATENCY + 12_345;
        assert_eq!(link.next_due(late), Some(late));
    });
}
