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
use model::drive::{run_in, Stats};
use model::steps::{from_bytes, generate, Kind, STEP_BYTES};
use model::world::Campaign;

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
/// that follows a close alert already taken for the peer's end. `t` is
/// one that TLS allows and need not reach, and that is excluded without
/// it: a reset after that close alert.
#[cfg(test)]
const EXPECTED: [&str; 10] = [
    //close shut send next Conn Secd data eos other Sent ShutD Closed dead pulled accept
    "UUUUUUUUUUUUUUR", // Free
    "RRRRRTUUUUUURUU", // Connecting
    "UUUUUTUUUUUUUUU", // Securing
    "RRRRUURRRRRURRU", // Up
    "R.RRUUUUUR.UURU", // Refused
    "RRRRUURRRRRU.RU", // Finishing
    "RRRRUUUTtRRU.RU", // HalfClosed
    "RRRR..RRRRRUURU", // Dying
    "....R.RRRRRRUUR", // Closing
    "RRRRUUUUUUUURUU", // Backoff
];

/// The same for the listener state table: `listen()`, `close_listener()`,
/// accept ok, accept failed, Closed, deadline. No run binds with a retry,
/// so none is ever `Binding`. A drain goes over every slot, free and
/// closing ones too, and finds nothing to stop there.
#[cfg(test)]
const LISTENERS: [&str; 4] = [
    "R.UUUU", // Free
    "U.UUU.", // Binding
    "URRRUR", // Listening
    "U..RRU", // Closing
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

/// Hold a counted table against what is wanted of it: `want` has a row
/// of marks for each of `rows` and a mark for each of `cols`. `T` and `t`
/// are for TLS, as [`EXPECTED`] says.
#[cfg(test)]
fn held(
    wrong: &mut Vec<String>,
    (table, rows, cols): (&str, &[&str], &[&str]),
    want: &[&str],
    hits: impl Fn(usize, usize) -> u32,
) {
    let tls = cfg!(feature = "tls-core");
    for (row, state) in rows.iter().enumerate() {
        for (col, event) in cols.iter().enumerate() {
            let (n, mark) = (hits(row, col), want[row].as_bytes()[col]);
            println!(
                "{table} {state:>12} | {event:<28} | {n:>8} | {}",
                mark as char
            );
            let wanted = match mark {
                b'T' if tls => b'R',
                b't' if tls => b'.',
                b'T' | b't' => b'U',
                other => other,
            };
            match wanted {
                b'R' if n == 0 => wrong.push(format!("{table} {state} / {event}: never reached")),
                b'U' if n > 0 => {
                    wrong.push(format!("{table} {state} / {event}: reached {n} times"));
                }
                _ => {}
            }
        }
    }
}

/// What the three tables hold against what was counted.
#[cfg(test)]
fn report(cover: &Coverage, phases: &PhaseCoverage) -> Vec<String> {
    let mut wrong = Vec::new();
    let conns = ("connection", &Coverage::STATES[..], &Coverage::EVENTS[..]);
    held(&mut wrong, conns, &EXPECTED, |r, c| cover.hits(r, c));
    let listeners = (
        "listener",
        &Coverage::LISTENER_STATES[..],
        &Coverage::LISTENER_EVENTS[..],
    );
    held(&mut wrong, listeners, &LISTENERS, |r, c| {
        cover.listener_hits(r, c)
    });
    if cfg!(feature = "tls-core") {
        let slots = (
            "phase",
            &PhaseCoverage::PHASES[..],
            &PhaseCoverage::EVENTS[..],
        );
        held(&mut wrong, slots, &PHASES, |r, c| phases.hits(r, c));
    }
    wrong
}

/// The runs of `seeds` in `campaign`, each of `STEPS` steps: what they did
/// in all. A run that fails names its campaign and seed, to be replayed.
#[cfg(test)]
fn campaign(campaign: Campaign, seeds: core::ops::Range<u64>) -> Stats {
    let mut total = Stats::default();
    for seed in seeds {
        let steps = generate(seed, STEPS);
        let outcome = std::panic::catch_unwind(|| run_in(campaign, seed, &steps, false));
        let stats = match outcome {
            Ok(Ok(stats)) => stats,
            Ok(Err(f)) => panic!(
                "{campaign:?}, seed {seed}, step {} of {STEPS}: {}",
                f.step, f.why
            ),
            Err(_) => panic!("{campaign:?}, seed {seed} of {STEPS} steps panicked"),
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
    total
}

/// The cells of the three tables that `total` reached though its table
/// excludes them.
#[cfg(test)]
fn excluded(total: &Stats) -> Vec<String> {
    let mut wrong = report(&total.cover, &total.phases);
    wrong.retain(|cell| cell.contains(": reached "));
    wrong
}

#[test]
fn random_operations_keep_what_a_caller_relies_on() {
    let total = campaign(Campaign::Usual, 0..SEEDS);
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
    let wrong = report(&total.cover, &total.phases);
    assert!(!cfg!(debug_assertions) || wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn every_fault_campaign_keeps_what_a_caller_relies_on() {
    for named in Campaign::all().iter().filter(|c| **c != Campaign::Usual) {
        let total = campaign(*named, 0..40);
        assert!(total.frames > 100, "{named:?}: {total:?}");
        let wrong = excluded(&total);
        assert!(
            !cfg!(debug_assertions) || wrong.is_empty(),
            "{named:?}: {wrong:#?}"
        );
    }
}

#[test]
fn the_same_seed_gives_the_same_trace() {
    // Not under flipped bits. A handshake carries random bytes, other
    // ones in every run, and what a flipped bit does there can depend on
    // the value it meets: the same seed then takes another course.
    for named in Campaign::all().iter().filter(|c| **c != Campaign::Flips) {
        let mut traces = Vec::new();
        for seed in 0..8 {
            let steps = generate(seed, STEPS);
            let trace = |()| match run_in(*named, seed, &steps, false) {
                Ok(stats) => stats.trace,
                Err(f) => panic!("{named:?}, seed {seed}, step {}: {}", f.step, f.why),
            };
            let (once, again) = (trace(()), trace(()));
            assert_eq!(once, again, "{named:?}, seed {seed}");
            traces.push(once);
        }
        // And another seed gives another.
        traces.sort_unstable();
        traces.dedup();
        assert_eq!(traces.len(), 8, "{named:?}");
    }
}

/// The seeds from `first` on, `count` of them, in `named`, in blocks. Each
/// block is announced on the standard error itself, past the test
/// harness: an allocation under the guard aborts the process, with
/// nothing said of the run it was in, and what the harness holds back is
/// lost with it. The last block announced is then where to look, with
/// `tcp_model --campaign NAME --seed N` over its seeds.
#[cfg(test)]
fn long_run(named: Campaign, first: u64, count: u64) {
    use std::io::Write;
    const BLOCK: u64 = 250;
    let mut at = first;
    while at < first + count {
        let end = (at + BLOCK).min(first + count);
        // Best effort: a line that is lost changes no result.
        drop(writeln!(
            std::io::stderr(),
            "tcp_model: {named:?}, seeds {at}..{end}"
        ));
        let total = campaign(named, at..end);
        let wrong = excluded(&total);
        assert!(
            !cfg!(debug_assertions) || wrong.is_empty(),
            "{named:?}, seeds {at}..{end}: {wrong:#?}"
        );
        at = end;
    }
}

/// CI's share: `cargo test -p longshore_tcp --test tcp_model -- --ignored
/// a_thousand`.
#[test]
#[ignore = "a thousand seeds: minutes, for CI"]
fn a_thousand_seeds_of_fault_campaigns() {
    let each = 1000 / Campaign::all().len() as u64 + 1;
    for named in Campaign::all() {
        long_run(*named, 1000, each);
    }
}

/// The nightly share, on every core: `cargo test -p longshore_tcp --test
/// tcp_model -- --ignored a_hundred_thousand`.
#[test]
#[ignore = "a hundred thousand seeds: the better part of an hour, nightly"]
fn a_hundred_thousand_seeds_of_fault_campaigns() {
    let cores = std::thread::available_parallelism().map_or(1, usize::from) as u64;
    let each = 100_000 / Campaign::all().len() as u64 / cores + 1;
    let workers: Vec<_> = (0..cores)
        .map(|core| {
            std::thread::spawn(move || {
                for named in Campaign::all() {
                    long_run(*named, 10_000 + core * each, each);
                }
            })
        })
        .collect();
    for worker in workers {
        assert!(worker.join().is_ok(), "a run failed: see above");
    }
}

#[test]
fn a_run_is_read_from_bytes_as_the_fuzzer_makes_them() {
    assert!(from_bytes(&[0; 7], 10).is_none());
    let mut bytes = 5u64.to_le_bytes().to_vec();
    // A send on the server set, then a turn on the client's, then seven
    // bytes of a step that is not whole.
    bytes.extend([3 << 1 | 1, 1, 0, 0, 0, 2, 0, 0, 0]);
    bytes.extend([10 << 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend([0; STEP_BYTES - 2]);
    let (seed, steps) = from_bytes(&bytes, 10).unwrap();
    assert_eq!((seed, steps.len()), (5, 2));
    let shown = format!("{steps:?}");
    assert!(
        shown.contains("kind: Send, server: true, a: 1, b: 2"),
        "{shown}"
    );
    assert!(shown.contains(&format!("kind: {:?}, server: false", Kind::Turn)));
    // And no more steps than asked for.
    assert_eq!(from_bytes(&bytes, 1).map(|run| run.1.len()), Some(1));
}
