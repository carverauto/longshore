// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Replays one run of the model test of `Net`, and cuts a failing run
//! down to the steps it needs.
//!
//!   cargo run -p longshore_tcp --example tcp_model -- --seed N [--steps M] [--campaign NAME] [--shrink] [--trace]
//!
//!   cargo run -p longshore_tcp --example tcp_model -- --input FILE [--shrink] [--trace]
//!
//! The second form replays a run from the bytes of FILE, as the fuzz
//! target `tcp_model_steps` reads them: a crash it wrote.
//!
//! The campaigns are the fault settings a run is under: usual (the
//! default), resets, slow, delays, refusals, and flips where TLS is
//! compiled in.
//!
//! An allocation under the guard is not a failure the tool can catch: it
//! aborts the process, here as in the test, and `--shrink` cannot cut
//! such a run down. The guard's own line and the seed given are what
//! there is.
//!
//! Run it as a debug build, as above: a failing run is caught as a panic
//! that unwinds, and much of what fails a run is a debug assertion. The
//! release profile aborts on a panic.

#[path = "../tests/tcp/model/mod.rs"]
mod model;

use model::drive::{run_in, Failure};
use model::steps::{from_bytes, generate, Step};
use model::world::Campaign;
use std::sync::Mutex;

/// What the set and the simulator do after they are built runs under
/// this: an allocation there aborts the run.
#[global_allocator]
static ALLOC: longshore_common::io::alloc_guard::Guard = longshore_common::io::alloc_guard::Guard;

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
fn outcome(run: (Campaign, u64), steps: &[Step]) -> Option<Failure> {
    match std::panic::catch_unwind(|| run_in(run.0, run.1, steps, false)) {
        Ok(Ok(_)) => None,
        Ok(Err(failure)) => Some(failure),
        Err(_) => Some(Failure {
            step: steps.len(),
            why: format!("panicked: {}", last_panic()),
        }),
    }
}

/// Drop steps, in ever smaller runs, while what is left still fails.
fn shrink(run: (Campaign, u64), mut steps: Vec<Step>) -> Vec<Step> {
    let mut chunk = steps.len() / 2;
    while chunk > 0 {
        let mut at = 0;
        while at < steps.len() {
            let mut fewer = steps.clone();
            let end = (at + chunk).min(fewer.len());
            drop(fewer.drain(at..end));
            if outcome(run, &fewer).is_some() {
                steps = fewer;
            } else {
                at += chunk;
            }
        }
        chunk /= 2;
    }
    steps
}

/// The campaign called `name`, in any case.
fn named(name: &str) -> Option<Campaign> {
    let all = Campaign::all().iter().copied();
    all.into_iter()
        .find(|c| format!("{c:?}").eq_ignore_ascii_case(name))
}

/// What to run: the campaign and the seed, and the steps of an input
/// file if one was named; the number of steps otherwise; and whether to
/// shrink and to trace.
type Asked = ((Campaign, u64), Option<Vec<Step>>, usize, bool, bool);

/// The run the bytes of the file at `path` describe.
fn input(path: &str) -> (u64, Vec<Step>) {
    let read = std::fs::read(path).ok();
    match read
        .as_deref()
        .and_then(|bytes| from_bytes(bytes, usize::MAX))
    {
        Some(run) => run,
        None => {
            eprintln!("tcp_model: {path} cannot be read, or is shorter than a seed");
            std::process::exit(2);
        }
    }
}

fn arguments() -> Asked {
    let (mut seed, mut count, mut cut, mut trace) = (0u64, 3000usize, false, false);
    let mut given = None;
    let mut campaign = Campaign::Usual;
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
            "--campaign" => match args.next().and_then(|name| named(&name)) {
                Some(named) => campaign = named,
                None => {
                    eprintln!("tcp_model: --campaign needs one of {:?}", Campaign::all());
                    std::process::exit(2);
                }
            },
            "--input" => match args.next() {
                Some(path) => {
                    let (from, steps) = input(&path);
                    (seed, given) = (from, Some(steps));
                }
                None => {
                    eprintln!("tcp_model: --input needs a file");
                    std::process::exit(2);
                }
            },
            "--shrink" => cut = true,
            "--trace" => trace = true,
            other => {
                eprintln!("tcp_model: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    ((campaign, seed), given, count, cut, trace)
}

fn main() {
    let (run, given, asked, cut, trace) = arguments();
    let (campaign, seed) = run;
    // A failing run's panic is kept and shown with its step, once.
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut said) = LAST_PANIC.lock() {
            *said = info.to_string();
        }
    }));
    let steps = given.unwrap_or_else(|| generate(seed, asked));
    let count = steps.len();
    if trace {
        // What each step did, up to where the run fails.
        drop(std::panic::catch_unwind(|| {
            run_in(campaign, seed, &steps, true)
        }));
    }
    let Some(failure) = outcome(run, &steps) else {
        println!("seed {seed}, {count} steps: ok");
        return;
    };
    println!(
        "seed {seed}: step {} of {count}: {}",
        failure.step, failure.why
    );
    if cut {
        let fewer = shrink(run, steps);
        println!("{} steps still fail:", fewer.len());
        for (at, step) in fewer.iter().enumerate() {
            println!("  {at}: {step:?}");
        }
        if let Some(last) = outcome(run, &fewer) {
            println!("step {}: {}", last.step, last.why);
        }
    }
    std::process::exit(1);
}
