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
//! are a step: what it does, on which side, and its two numbers
//! (`model::steps::from_bytes`). A crash is replayed, traced and cut down
//! with the model's own tool:
//!
//!   cargo run -p longshore_tcp --example tcp_model -- --input FILE --trace --shrink

use libfuzzer_sys::fuzz_target;
// The model itself, from the test's own source: see fuzz/Cargo.toml for
// why sources are compiled in place. Parts of it serve the test and its
// replay tool only.
#[allow(dead_code, unused_imports)]
#[path = "../../crates/longshore_tcp/tests/tcp/model/mod.rs"]
mod model;

use model::drive::run_in;
use model::steps::{from_bytes, STEP_BYTES};
use model::world::Campaign;

/// The most steps of a run: what libFuzzer's longest input, 4096 bytes
/// unless told otherwise, holds behind the seed.
const STEPS: usize = (4096 - 8) / STEP_BYTES;

fuzz_target!(|data: &[u8]| {
    let Some((seed, steps)) = from_bytes(data, STEPS) else {
        return;
    };
    if let Err(failure) = run_in(Campaign::Usual, seed, &steps, false) {
        panic!("step {} of {}: {}", failure.step, steps.len(), failure.why);
    }
});
