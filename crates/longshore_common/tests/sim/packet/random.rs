// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Seeded random calls on a link under every fault at once, with the
//! invariants of its design held after each: every slot in one place,
//! nothing made or lost unaccounted, nothing changed, nothing early or
//! too late, and never three of a packet.

use super::{fill, number, LATENCY, MAX};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::sim::prng::Prng;
use longshore_common::sim::{End, PacketFaults, PacketLink, PacketSizes};

const SLOTS: u32 = 12;
const DELAY: u64 = 250_000;

/// What the test knows of a packet it sent: when the turn after it was,
/// and how often it has arrived.
#[derive(Clone, Copy, Default)]
struct Sent {
    departed: Option<u64>,
    arrived: u8,
}

struct Run {
    link: PacketLink,
    now: u64,
    /// Each end's packets, by number.
    sent: [Vec<Sent>; 2],
    refused: u64,
}

impl Run {
    fn send(&mut self, from: End) {
        let known = &mut self.sent[usize::from(from == End::B)];
        if known.len() == known.capacity() {
            return;
        }
        let mut packet = [0; MAX];
        let len = fill(&mut packet, from, known.len() as u64);
        let before = self.link.counts();
        match sealed(|| self.link.send(from, &packet[..len])) {
            Ok(()) => known.push(Sent::default()),
            Err(_) => {
                // A refusal changes nothing.
                assert_eq!(self.link.counts(), before);
                self.refused += 1;
            }
        }
    }

    fn turn(&mut self) {
        sealed(|| self.link.turn(self.now));
        for known in &mut self.sent {
            for packet in known.iter_mut().filter(|p| p.departed.is_none()) {
                packet.departed = Some(self.now);
            }
        }
    }

    fn recv(&mut self, at: End) {
        let mut into = [0; MAX];
        let Some(len) = sealed(|| self.link.recv(at, &mut into)) else {
            return;
        };
        let id = number(&into[..len], at);
        let from = &mut self.sent[usize::from(at == End::A)];
        let packet = &mut from[id as usize];
        packet.arrived += 1;
        assert!(packet.arrived <= 2, "packet {id} three times");
        // Not before its time. (How late it may be is held where turns
        // come at a steady pace: here it lies in the receive queue until
        // the next turn and the next receive, whenever those are.)
        let departed = packet.departed;
        assert!(
            departed.is_some(),
            "packet {id} before the turn that sent it"
        );
        let earliest = departed.map_or(u64::MAX, |when| when + LATENCY);
        assert!(self.now >= earliest, "packet {id} early");
    }

    /// Every slot is in one place, and every packet accounted for.
    fn check(&self) {
        let counts = self.link.counts();
        let places = counts.places();
        assert_eq!(places.iter().sum::<u32>(), SLOTS, "{counts:?}");
        let waiting = u64::from(places[1] + places[2] + places[3]);
        assert_eq!(
            counts.sent() + counts.duplicated(),
            counts.received() + counts.lost() + counts.overrun() + waiting,
            "{counts:?}"
        );
    }
}

/// `calls` random calls with `seed`; what the link counted.
fn run(seed: u64, calls: u32) -> (u64, u64, u64, u64) {
    let faults = PacketFaults::NONE
        .with_loss(80_000)
        .with_duplicate(80_000)
        .with_reorder(80_000)
        .with_delay(300_000, DELAY);
    let sizes = PacketSizes::new(SLOTS, MAX as u32).with_queue(5);
    let room = || Vec::with_capacity(calls as usize);
    let mut run = Run {
        link: PacketLink::new(seed, faults, sizes),
        now: 0,
        sent: [room(), room()],
        refused: 0,
    };
    let mut choose = Prng::new(seed ^ 0x5eed);
    for _ in 0..calls {
        match choose.below(10) {
            0..=3 => run.send(if choose.chance(500_000) {
                End::A
            } else {
                End::B
            }),
            4..=5 => run.turn(),
            6..=8 => run.recv(if choose.chance(500_000) {
                End::A
            } else {
                End::B
            }),
            _ => run.now += choose.below(400_000),
        }
        run.check();
    }
    // To the end: everything on its way arrives or is dropped.
    while let Some(due) = run.link.next_due(run.now) {
        run.now = run.now.max(due);
        run.turn();
        run.recv(End::A);
        run.recv(End::B);
        run.check();
    }
    for at in [End::A, End::B] {
        for _ in 0..SLOTS {
            run.recv(at);
        }
    }
    run.check();
    let counts = run.link.counts();
    assert_eq!(counts.places(), [SLOTS, 0, 0, 0], "seed {seed}");
    (
        counts.lost(),
        counts.duplicated(),
        counts.overrun(),
        run.refused,
    )
}

#[test]
fn random_calls_keep_every_packet_in_one_place_and_accounted_for() {
    let mut total = (0, 0, 0, 0);
    for seed in 0..300 {
        let (lost, twice, overrun, refused) = run(seed, 3000);
        total = (
            total.0 + lost,
            total.1 + twice,
            total.2 + overrun,
            total.3 + refused,
        );
    }
    // The runs met every fault and both ways of running out.
    assert!(total.0 > 1000 && total.1 > 1000, "{total:?}");
    assert!(total.2 > 100 && total.3 > 1000, "{total:?}");
}
