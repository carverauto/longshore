// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The hand-offs between callers and I/O threads, run in every order a
//! model checker can reach.
//!
//! Each test is a few threads: callers, and one thread per lane running the
//! real [`Worker`] over the simulator. `loom` runs the test again and again,
//! switching threads at every lock, atomic, condition variable and park in
//! `longshore::client`, and fails if any order ends in a panic or leaves a
//! thread waiting for ever. A lost wake-up shows up as the second.
//!
//! This file is empty in an ordinary build. Run it with
//! `scripts/test-model.sh`.

#![cfg(longshore_model)]

#[path = "../store/common/mod.rs"]
mod common;
#[path = "model/machine.rs"]
mod machine;

use longshore::client::Error;
use machine::*;

#[test]
fn two_blocking_callers_share_one_operation_slot() {
    model(|| {
        let w = World::new(&[Fate::Serve], 1, 1);
        let other = w.client.clone();
        let t = spawn(move || assert_eq!(other.erase("a").wait(), Ok(())));
        assert_eq!(w.client.erase("b").wait(), Ok(()));
        t.join().unwrap();
        w.end();
    });
}

#[test]
fn a_future_and_a_blocking_caller_share_one_operation_slot() {
    model(|| {
        let w = World::new(&[Fate::Serve], 1, 1);
        let other = w.client.clone();
        let t = spawn(move || assert_eq!(block_on(other.erase("a")), Ok(())));
        assert_eq!(w.client.erase("b").wait(), Ok(()));
        t.join().unwrap();
        w.end();
    });
}

#[test]
fn two_callers_share_one_buffer() {
    model(|| {
        let w = World::new(&[Fate::Serve], 2, 1);
        let other = w.client.clone();
        let t = spawn(move || {
            let body = block_on(other.buffer());
            let (stored, _body) = block_on(other.store("a", body, 100));
            assert_eq!(stored, Ok(()));
        });
        let body = w.client.buffer_blocking();
        let (stored, body) = w.client.store("b", body, 100).wait();
        assert_eq!(stored, Ok(()));
        drop(body);
        t.join().unwrap();
        w.end();
    });
}

#[test]
fn an_operation_dropped_unfinished_gives_its_slot_to_the_next_caller() {
    model(|| {
        let w = World::new(&[Fate::Serve], 1, 1);
        let other = w.client.clone();
        let t = spawn(move || assert_eq!(other.erase("b").wait(), Ok(())));
        // Sent if the slot was free, queued for it if not, then let go.
        let mut dropped = Box::pin(w.client.erase("a"));
        let _ = poll_once(dropped.as_mut());
        drop(dropped);
        t.join().unwrap();
        assert_eq!(w.client.erase("c").wait(), Ok(()));
        w.end();
    });
}

#[test]
fn a_waiter_that_gives_up_passes_its_turn_on() {
    model_within(1, || {
        let w = World::new(&[Fate::Serve], 1, 1);
        let (second, third) = (w.client.clone(), w.client.clone());
        let gives_up = spawn(move || {
            let mut waiting = Box::pin(second.erase("b"));
            let _ = poll_once(waiting.as_mut());
        });
        let waits = spawn(move || assert_eq!(third.erase("c").wait(), Ok(())));
        assert_eq!(w.client.erase("a").wait(), Ok(()));
        gives_up.join().unwrap();
        waits.join().unwrap();
        w.end();
    });
}

/// Ok, or closed because the lane failed first.
fn ended(result: Result<(), Error>) {
    assert!(matches!(result, Ok(()) | Err(Error::Closed)), "{result:?}");
}

#[test]
fn callers_of_a_lane_that_fails_all_get_an_answer() {
    for after_turns in [0, 1] {
        model(move || {
            let fate = Fate::Fail {
                quiet: true,
                after_turns,
            };
            let w = World::new(&[fate], 1, 1);
            let other = w.client.clone();
            let t = spawn(move || ended(other.erase("a").wait()));
            ended(block_on(w.client.erase("b")));
            t.join().unwrap();
            w.end();
        });
    }
}

#[test]
fn callers_holding_buffers_of_a_lane_that_cannot_be_stopped_all_get_an_answer() {
    for after_turns in [0, 1] {
        model(move || {
            let fate = Fate::Fail {
                quiet: false,
                after_turns,
            };
            let w = World::new(&[fate], 2, 1);
            let other = w.client.clone();
            let store = |client: longshore::client::Client, key: &str| {
                let body = client.buffer_blocking();
                let void = body.is_empty();
                let (stored, _body) = client.store(key, body, 0).wait();
                if void {
                    assert_eq!(stored, Err(Error::Closed));
                } else {
                    ended(stored);
                }
            };
            let t = spawn(move || store(other, "a"));
            store(w.client.clone(), "b");
            t.join().unwrap();
            w.end();
        });
    }
}

#[test]
fn a_caller_waiting_for_a_buffer_when_the_last_lane_fails_stops_waiting() {
    for quiet in [true, false] {
        model(move || {
            let fate = Fate::Fail {
                quiet,
                after_turns: 0,
            };
            let w = World::new(&[fate], 1, 1);
            // The lane's one buffer, unless the lane has failed already.
            let held = w.client.buffer_blocking();
            let other = w.client.clone();
            // No buffer will come back, so this ends only because the lane
            // closing wakes it, and then with a void buffer.
            let t = spawn(move || {
                let waited_for = other.buffer_blocking();
                assert!(waited_for.is_empty() || !other.buffer_blocking().is_empty());
            });
            t.join().unwrap();
            drop(held);
            w.end();
        });
    }
}

#[test]
fn with_one_lane_failing_an_erase_ends_and_the_next_runs_on_the_other() {
    model_within(1, || {
        let failing = Fate::Fail {
            quiet: true,
            after_turns: 0,
        };
        let w = World::new(&[failing, Fate::Serve], 1, 1);
        let other = w.client.clone();
        let t = spawn(move || ended(other.erase("a").wait()));
        ended(w.client.erase("b").wait());
        t.join().unwrap();
        w.failed(0);
        assert_eq!(w.client.erase("c").wait(), Ok(()));
        w.end();
    });
}
