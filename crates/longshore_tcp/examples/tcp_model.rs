// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Replays one run of the model test of `Net`, and cuts a failing run
//! down to the steps it needs.
//!
//!   cargo run -p longshore_tcp --example tcp_model -- --seed N [--steps M] [--shrink] [--trace]

#[path = "../tests/tcp/model/mod.rs"]
mod model;

use model::drive::{run, run_traced, Failure};
use model::steps::{generate, Step};

/// What `steps` come to with `seed`: where and how they fail, if they do.
/// A panic inside the set counts as a failure at an unknown step.
fn outcome(seed: u64, steps: &[Step]) -> Option<Failure> {
    match std::panic::catch_unwind(|| run(seed, steps)) {
        Ok(Ok(_)) => None,
        Ok(Err(failure)) => Some(failure),
        Err(_) => Some(Failure {
            step: steps.len(),
            why: "panicked".to_string(),
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

fn main() {
    let (mut seed, mut count, mut cut, mut trace) = (0u64, 3000usize, false, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let number = |value: Option<String>| value.and_then(|v| v.parse::<u64>().ok());
        match arg.as_str() {
            "--seed" => seed = number(args.next()).unwrap_or(seed),
            "--steps" => count = number(args.next()).map_or(count, |n| n as usize),
            "--shrink" => cut = true,
            "--trace" => trace = true,
            other => {
                eprintln!("tcp_model: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    // The panics of failing runs are reported below, not one by one.
    std::panic::set_hook(Box::new(|_| {}));
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
