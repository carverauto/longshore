// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The model test of `Net`, over many seeds. A failure names its seed and
//! the step it failed at; `cargo run -p longshore_tcp --example tcp_model
//! -- --seed N --steps M --shrink` replays it and cuts it down.

#[path = "model/mod.rs"]
mod model;

use longshore_tcp::Coverage;

/// What the set and the simulator do after they are built runs under
/// this: an allocation there aborts the run.
#[global_allocator]
static ALLOC: longshore_common::io::alloc_guard::Guard = longshore_common::io::alloc_guard::Guard;
use model::drive::{run, Stats};
use model::steps::generate;

/// Steps of one run, and seeds in `cargo test`.
#[cfg(test)]
const STEPS: usize = 3000;
#[cfg(test)]
const SEEDS: u64 = 150;

/// What the runs must reach of the connection state table, a row per
/// state and a column per event, in the order of `Coverage::STATES` and
/// `Coverage::EVENTS`: `R` a cell the runs must reach, `U` one that must
/// never be reached, `.` one the table allows and these runs need not
/// reach, `T` one that must be reached where TLS is compiled in and
/// never where it is not: a handshake's end, and the end of the stream
/// that follows a close alert already taken for the peer's end.
#[cfg(test)]
const EXPECTED: [&str; 10] = [
    //close shut send next Conn Secd data eos other Sent ShutD Closed dead pulled accept
    "UUUUUUUUUUUUUUR", // Free
    "RRRRRTUUUUUURUU", // Connecting
    "UUUUUTUUUUUUUUU", // Securing
    "RRRRUURRRRRURRU", // Up
    "R.RRUUUUUR.UURU", // Refused
    "RRRRUURRRRRU.RU", // Finishing
    "RRRRUUUTURRU.RU", // HalfClosed
    "RRRR..RRRRRUURU", // Dying
    "....R.RRRRRRUUR", // Closing
    "RRRRUUUUUUUURUU", // Backoff
];

#[cfg(test)]
fn report(cover: &Coverage) -> Vec<String> {
    let mut wrong = Vec::new();
    for (row, state) in Coverage::STATES.iter().enumerate() {
        let want = EXPECTED[row].as_bytes();
        for (col, event) in Coverage::EVENTS.iter().enumerate() {
            let hits = cover.hits(row, col);
            println!(
                "{state:>10} | {event:<24} | {hits:>8} | {}",
                want[col] as char
            );
            let tls = cfg!(feature = "tls-core");
            let wanted = match want[col] {
                b'T' if tls => b'R',
                b'T' => b'U',
                other => other,
            };
            match wanted {
                b'R' if hits == 0 => wrong.push(format!("{state} / {event}: never reached")),
                b'U' if hits > 0 => wrong.push(format!("{state} / {event}: reached {hits} times")),
                _ => {}
            }
        }
    }
    wrong
}

#[test]
fn random_operations_keep_what_a_caller_relies_on() {
    let mut total = Stats::default();
    for seed in 0..SEEDS {
        let steps = generate(seed, STEPS);
        let outcome = std::panic::catch_unwind(|| run(seed, &steps));
        let stats = match outcome {
            Ok(Ok(stats)) => stats,
            Ok(Err(f)) => panic!("seed {seed}, step {} of {STEPS}: {}", f.step, f.why),
            Err(_) => panic!("seed {seed} of {STEPS} steps panicked"),
        };
        total.connects += stats.connects;
        total.accepted += stats.accepted;
        total.refused += stats.refused;
        total.frames += stats.frames;
        total.protected_frames += stats.protected_frames;
        total.relayed += stats.relayed;
        total.finished += stats.finished;
        total.closed_orderly += stats.closed_orderly;
        total.closed_otherwise += stats.closed_otherwise;
        total.went_down += stats.went_down;
        total.full += stats.full;
        total.stale_calls += stats.stale_calls;
        total.cover.add(&stats.cover);
    }
    // The runs did what they are for.
    assert!(total.accepted > 100 && total.frames > 1000, "{total:?}");
    assert!(
        !cfg!(feature = "tls-core") || total.protected_frames > 300,
        "{total:?}"
    );
    assert!(
        total.finished > 0 && total.relayed > 0 && total.full > 0,
        "{total:?}"
    );
    // And were where the state table says a connection can be, and
    // nowhere it says one cannot.
    // (The cells are counted in debug builds only.)
    let wrong = report(&total.cover);
    assert!(!cfg!(debug_assertions) || wrong.is_empty(), "{wrong:#?}");
}
