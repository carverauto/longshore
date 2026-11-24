// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The model test of `Net`, over many seeds. A failure names its seed and
//! the step it failed at; `cargo run -p longshore_tcp --example tcp_model
//! -- --seed N --steps M --shrink` replays it and cuts it down.

#[path = "model/mod.rs"]
mod model;

use model::drive::{run, Stats};
use model::steps::generate;

/// Steps of one run, and seeds in `cargo test`.
#[cfg(test)]
const STEPS: usize = 3000;
#[cfg(test)]
const SEEDS: u64 = 150;

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
        total.relayed += stats.relayed;
        total.finished += stats.finished;
        total.closed_orderly += stats.closed_orderly;
        total.closed_otherwise += stats.closed_otherwise;
        total.full += stats.full;
        total.stale_calls += stats.stale_calls;
    }
    println!("{total:?}");
    // The runs did what they are for.
    assert!(total.accepted > 100 && total.frames > 1000, "{total:?}");
    assert!(
        total.finished > 0 && total.relayed > 0 && total.full > 0,
        "{total:?}"
    );
}
