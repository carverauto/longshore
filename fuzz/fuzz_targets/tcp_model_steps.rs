// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! The model test of the TCP connection set, with its steps made from the
//! fuzzer's bytes where the test makes them from a seed: random calls on
//! a client set and a server set on the simulator, checked after every
//! step against what a caller may rely on. A run that fails is a crash.
//!
//! The first eight bytes are the simulator's seed; each nine after them
//! are a step: what it does, on which side, and its two numbers.

use libfuzzer_sys::fuzz_target;
// The model itself, from the test's own source: see fuzz/Cargo.toml for
// why sources are compiled in place. Parts of it serve the test and its
// replay tool only.
#[allow(dead_code, unused_imports)]
#[path = "../../crates/longshore_tcp/tests/tcp/model/mod.rs"]
mod model;

use model::drive::run_in;
use model::steps::{Kind, Step};
use model::world::Campaign;

/// The most steps of a run: enough for connections to come up, carry
/// frames and end, and few enough for many runs a second.
const STEPS: usize = 600;

fn kind(of: u8) -> Kind {
    match of % 10 {
        0 => Kind::Turn,
        1 => Kind::PullEvents,
        2 => Kind::PullFrames,
        3 => Kind::Send,
        4 => Kind::Connect,
        5 => Kind::Listen,
        6 => Kind::Close,
        7 => Kind::CloseListener,
        8 => Kind::Shutdown,
        _ => Kind::Release,
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((seed, rest)) = data.split_first_chunk::<8>() else {
        return;
    };
    let steps: Vec<Step> = rest
        .chunks_exact(9)
        .take(STEPS)
        .map(|s| Step {
            // The high bits choose the kind, the low one the side.
            kind: kind(s[0] >> 1),
            server: s[0] & 1 == 1,
            a: u32::from_le_bytes([s[1], s[2], s[3], s[4]]),
            b: u32::from_le_bytes([s[5], s[6], s[7], s[8]]),
        })
        .collect();
    if let Err(failure) = run_in(Campaign::Usual, u64::from_le_bytes(*seed), &steps, false) {
        panic!("step {} of {}: {}", failure.step, steps.len(), failure.why);
    }
});
