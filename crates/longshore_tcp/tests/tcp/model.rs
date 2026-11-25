// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The model test of `Net`, over many seeds. A failure names its seed and
//! the step it failed at; `cargo run -p longshore_tcp --example tcp_model
//! -- --seed N --steps M --shrink` replays it and cuts it down.

#[path = "model/mod.rs"]
mod model;

use longshore_tcp::{Coverage, PhaseCoverage};

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

/// The same for the listener state table: `listen()`, `close_listener()`,
/// accept ok, accept failed, Closed, deadline. No run binds with a retry,
/// so none is ever `Binding`.
#[cfg(test)]
const LISTENERS: [&str; 4] = [
    "RUUUUU", // Free
    "U.UUU.", // Binding
    "URRRUR", // Listening
    "UU.RRU", // Closing
];

/// And for the TLS phase table of the kernel transport, where TLS is
/// compiled in: `secure`, a send or receive completion, `shutdown_write`,
/// close submitted, Closed.
#[cfg(test)]
const PHASES: [&str; 6] = [
    "R..RR", // Plain
    "URURU", // Handshake
    "URU.R", // Aborted
    "URRRR", // Secure
    "URU.U", // Alerting
    "U.U..", // AlertAborted
];

/// Hold `hits` of the cell `name` against what the table wants of it.
#[cfg(test)]
fn judge(wrong: &mut Vec<String>, name: core::fmt::Arguments<'_>, hits: u32, want: u8) {
    println!("{name} | {hits:>8} | {}", want as char);
    match want {
        b'R' if hits == 0 => wrong.push(format!("{name}: never reached")),
        b'U' if hits > 0 => wrong.push(format!("{name}: reached {hits} times")),
        _ => {}
    }
}

#[cfg(test)]
fn report_others(cover: &Coverage, phases: &PhaseCoverage) -> Vec<String> {
    let mut wrong = Vec::new();
    for (row, state) in Coverage::LISTENER_STATES.iter().enumerate() {
        for (col, event) in Coverage::LISTENER_EVENTS.iter().enumerate() {
            let name = format_args!("listener {state:>9} | {event:<26}");
            let want = LISTENERS[row].as_bytes()[col];
            judge(&mut wrong, name, cover.listener_hits(row, col), want);
        }
    }
    if !cfg!(feature = "tls-core") {
        return wrong;
    }
    for (row, phase) in PhaseCoverage::PHASES.iter().enumerate() {
        for (col, event) in PhaseCoverage::EVENTS.iter().enumerate() {
            let name = format_args!("phase {phase:>12} | {event:<28}");
            let want = PHASES[row].as_bytes()[col];
            judge(&mut wrong, name, phases.hits(row, col), want);
        }
    }
    wrong
}

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
        total.phases.add(&stats.phases);
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
    let mut wrong = report(&total.cover);
    wrong.extend(report_others(&total.cover, &total.phases));
    assert!(!cfg!(debug_assertions) || wrong.is_empty(), "{wrong:#?}");
}
