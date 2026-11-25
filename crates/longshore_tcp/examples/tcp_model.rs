// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Replays one run of the model test of `Net`, and cuts a failing run
//! down to the steps it needs.
//!
//!   cargo run -p longshore_tcp --example tcp_model -- --seed N [--steps M] [--shrink] [--trace]
//!
//! Run it as a debug build, as above: a failing run is caught as a panic
//! that unwinds, and much of what fails a run is a debug assertion. The
//! release profile aborts on a panic.

#[path = "../tests/tcp/model/mod.rs"]
mod model;

use model::drive::{run, run_traced, Failure};
use model::steps::{generate, Step};
use std::sync::Mutex;

/// What the last panic said, kept by the hook `main` installs: the many
/// runs of a shrink would otherwise each print theirs.
static LAST_PANIC: Mutex<String> = Mutex::new(String::new());

fn last_panic() -> String {
    LAST_PANIC
        .lock()
        .map_or_else(|_| String::new(), |said| said.clone())
}

/// What `steps` come to with `seed`: where and how they fail, if they do.
/// A panic inside the set counts as a failure at an unknown step.
fn outcome(seed: u64, steps: &[Step]) -> Option<Failure> {
    match std::panic::catch_unwind(|| run(seed, steps)) {
        Ok(Ok(_)) => None,
        Ok(Err(failure)) => Some(failure),
        Err(_) => Some(Failure {
            step: steps.len(),
            why: format!("panicked: {}", last_panic()),
        }),
    }
}

/// Drop steps, in ever smaller runs, while what is left still fails.
fn shrink(seed: u64, mut steps: Vec<Step>) -> Vec<Step> {
    let mut chunk = steps.len() / 2;
    while chunk > 0 {
        let mut at = 0;
        while at < steps.len() {
            let mut fewer = steps.clone();
            let end = (at + chunk).min(fewer.len());
            drop(fewer.drain(at..end));
            if outcome(seed, &fewer).is_some() {
                steps = fewer;
            } else {
                at += chunk;
            }
        }
        chunk /= 2;
    }
    steps
}

/// The seed, the number of steps, and whether to shrink and to trace.
fn arguments() -> (u64, usize, bool, bool) {
    let (mut seed, mut count, mut cut, mut trace) = (0u64, 3000usize, false, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = || match args.next().map(|v| v.parse::<u64>()) {
            Some(Ok(n)) => n,
            _ => {
                eprintln!("tcp_model: {arg} needs a number");
                std::process::exit(2);
            }
        };
        match arg.as_str() {
            "--seed" => seed = number(),
            "--steps" => count = number() as usize,
            "--shrink" => cut = true,
            "--trace" => trace = true,
            other => {
                eprintln!("tcp_model: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    (seed, count, cut, trace)
}

fn main() {
    let (seed, count, cut, trace) = arguments();
    // A failing run's panic is kept and shown with its step, once.
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut said) = LAST_PANIC.lock() {
            *said = info.to_string();
        }
    }));
    let steps = generate(seed, count);
    if trace {
        // What each step did, up to where the run fails.
        drop(std::panic::catch_unwind(|| run_traced(seed, &steps, true)));
    }
    let Some(failure) = outcome(seed, &steps) else {
        println!("seed {seed}, {count} steps: ok");
        return;
    };
    println!(
        "seed {seed}: step {} of {count}: {}",
        failure.step, failure.why
    );
    if cut {
        let fewer = shrink(seed, steps);
        println!("{} steps still fail:", fewer.len());
        for (at, step) in fewer.iter().enumerate() {
            println!("  {at}: {step:?}");
        }
        if let Some(last) = outcome(seed, &fewer) {
            println!("step {}: {}", last.step, last.why);
        }
    }
    std::process::exit(1);
}
