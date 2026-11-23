// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The benign faults do something. An echo that survives short writes,
//! partial reads and delays proves nothing if none of them ever happens, so
//! each is shown to leave its mark on the run: more sends, more receives,
//! more ticks than the same echo without it.

use longshore_common::io::{Completions, Limits};
use longshore_common::sim::scenario::{echo, EchoStats};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const LIMITS: Limits = Limits {
    sockets_max: 4,
    buffers: 3,
    buffer_size: 4096,
    ops_max: 4,
    ..Limits::BASE
};
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const TOTAL: u64 = 300_000;
const CHUNK: u32 = 4096;
/// Sends or receives it takes to move `TOTAL` bytes a chunk at a time.
const CHUNKS: u64 = TOTAL.div_ceil(CHUNK as u64);

#[cfg(test)]
fn run(seed: u64, faults: Faults) -> EchoStats {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut out = Completions::new(8);
    echo(&mut io, ADDR, TOTAL, CHUNK, false, &mut out)
        .unwrap_or_else(|e| panic!("seed {seed}, {faults:?}: {e:?}"))
}

#[test]
fn without_faults_every_send_and_receive_moves_a_whole_chunk() {
    for seed in 0..20 {
        let plain = run(seed, Faults::default());
        assert_eq!(plain.sends, CHUNKS, "seed {seed}");
        assert!(plain.recvs >= CHUNKS, "seed {seed}: {plain:?}");
    }
}

#[test]
fn short_sends_take_more_sends() {
    for seed in 0..20 {
        let plain = run(seed, Faults::default());
        let faulty = run(
            seed,
            Faults {
                short_send: 300_000,
                ..Faults::default()
            },
        );
        assert!(
            faulty.sends > plain.sends,
            "seed {seed}: {faulty:?} against {plain:?}"
        );
    }
}

#[test]
fn partial_receives_take_more_receives() {
    for seed in 0..20 {
        let plain = run(seed, Faults::default());
        let faulty = run(
            seed,
            Faults {
                partial_recv: 300_000,
                ..Faults::default()
            },
        );
        assert!(
            faulty.recvs > plain.recvs,
            "seed {seed}: {faulty:?} against {plain:?}"
        );
        // Sends are not compared: a reader that falls behind fills the
        // socket, and a full socket takes less than it is offered.
    }
}

#[test]
fn delays_take_more_ticks_and_change_nothing_else() {
    for seed in 0..20 {
        let plain = run(seed, Faults::default());
        let faulty = run(
            seed,
            Faults {
                delay: 200_000,
                ..Faults::default()
            },
        );
        assert!(
            faulty.ticks > plain.ticks,
            "seed {seed}: {faulty:?} against {plain:?}"
        );
        assert_eq!(faulty.sends, CHUNKS, "seed {seed}");
    }
}

/// A probability of zero is never, and of a million in a million is always.
#[test]
fn fault_probabilities_have_their_ends() {
    use longshore_common::sim::prng::Prng;
    let mut rng = Prng::new(7);
    assert!((0..10_000).all(|_| !rng.chance(0)));
    assert!((0..10_000).all(|_| rng.chance(1_000_000)));
    let hits = (0..100_000).filter(|_| rng.chance(250_000)).count();
    assert!((24_000..26_000).contains(&hits), "{hits} of 100,000");
    assert!((0..10_000).all(|_| rng.below(7) < 7));
    // The same seed gives the same stream, another seed another.
    let stream = |seed| {
        let mut r = Prng::new(seed);
        [r.next_u64(), r.next_u64(), r.next_u64()]
    };
    assert_eq!(stream(1), stream(1));
    assert_ne!(stream(1), stream(2));
}
