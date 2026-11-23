// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property test (Hegel) for the retry policy as a whole: whatever run of
//! retryable failures a store meets, it ends as the budget says it must,
//! having waited as long as the backoff says and no longer.

#[cfg(test)]
mod common;
use common::*;
use hegel::generators as gs;
use hegel::TestCase;

/// The failures a store can be scripted to meet, and whether each asks for
/// a delay before the next attempt.
const FAILURES: [(Behavior, bool); 7] = [
    (Behavior::Status(408), false),
    (Behavior::Status(500), false),
    (Behavior::Status(502), false),
    (Behavior::Status(504), false),
    (Behavior::CloseBeforeResponse, false),
    (Behavior::Status(429), true),
    (Behavior::Status(503), true),
];

#[hegel::test]
fn a_run_of_failures_ends_as_the_budget_and_the_backoff_say(tc: TestCase) {
    let attempts_max = tc.draw(gs::integers::<u32>().min_value(1).max_value(6));
    let script: Vec<(Behavior, bool)> = (0..tc
        .draw(gs::integers::<usize>().min_value(0).max_value(8)))
        .map(|_| {
            FAILURES[tc.draw(
                gs::integers::<usize>()
                    .min_value(0)
                    .max_value(FAILURES.len() - 1),
            )]
        })
        .collect();

    let mut w = world(
        tc.draw(gs::integers::<u64>()),
        Faults::default(),
        attempts_max,
    );
    w.fill_tx(100, 0);
    for (behavior, _) in &script {
        w.server.script(*behavior);
    }
    let t0 = w.io.now_ns();
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::None)
        .unwrap();
    let done = w.last();
    let waited = w.io.now_ns() - t0;

    // Each attempt meets the next failure until they run out or the budget does.
    let failed = script.len().min(attempts_max as usize);
    let stored = script.len() < attempts_max as usize;
    let expected = if stored {
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: failed as u32 + 1,
        }
    } else {
        Done {
            token: 1,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: attempts_max,
        }
    };
    assert_eq!(done, expected, "{script:?} with {attempts_max} attempts");
    assert_eq!(w.server.object_len("/k"), stored.then_some(100));
    assert_eq!(w.s3.in_progress(), 0);
    assert!(w.s3.poll().is_none());

    // A slow-down answer is followed by a wait only if an attempt follows
    // it: 100 ms, doubling, and never more than 800 ms.
    let delays = script[..failed]
        .iter()
        .enumerate()
        .filter(|(i, (_, delayed))| *delayed && i + 1 < attempts_max as usize)
        .count() as u64;
    let backoff: u64 = (0..delays).map(|k| ((100 * MS) << k).min(800 * MS)).sum();
    // Each wait ends on the next 10 ms tick; the requests take no time to speak of.
    let slack = (delays + 1) * 10 * MS + 10 * MS;
    assert!(
        (backoff..backoff + slack).contains(&waited),
        "{script:?} with {attempts_max} attempts: waited {} ms for a backoff of {} ms",
        waited / MS,
        backoff / MS
    );
}
