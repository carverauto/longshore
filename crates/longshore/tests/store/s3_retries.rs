// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store against the in-tree HTTP server on one simulated `Io`, with
//! heap allocation forbidden while operations run.

mod common;
use common::*;

#[test]
fn slow_down_responses_are_retried_after_a_backoff() {
    let mut w = world(3, Faults::default(), 5);
    w.fill_tx(100, 0);
    w.server.script(Behavior::Status(503));
    w.server.script(Behavior::Status(503));
    let t0 = w.io.now_ns();
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 3
        }
    );
    let waited = (w.io.now_ns() - t0) / MS;
    // Backoff is 100 ms then 200 ms, observed on a 10 ms tick.
    assert!(
        (300..360).contains(&waited),
        "waited {waited} ms, expected about 300"
    );
}

#[test]
fn server_errors_retry_immediately_and_give_up_at_the_budget() {
    let mut w = world(4, Faults::default(), 3);
    w.fill_tx(100, 0);
    w.server.script(Behavior::Status(500));
    w.server.script(Behavior::CloseBeforeResponse);
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 3
        },
        "500 then a dropped connection, then success"
    );

    for _ in 0..3 {
        w.server.script(Behavior::Status(500));
    }
    w.s3.store(&mut w.io, 2, "k2", TX, 100, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: 3
        }
    );
    assert_eq!(w.server.object_len("/k2"), None);
}

#[test]
fn definite_refusals_are_fatal_without_retry() {
    for status in [400u16, 403, 405, 301] {
        let mut w = world(5, Faults::default(), 5);
        w.fill_tx(10, 0);
        w.server.script(Behavior::Status(status));
        w.s3.store(&mut w.io, 1, "k", TX, 10, Checksum::None)
            .unwrap();
        assert_eq!(
            w.last(),
            Done {
                token: 1,
                outcome: Outcome::Fatal(Fatal::Status(status)),
                attempts: 1
            },
            "status {status}"
        );
    }
}

#[test]
fn a_stalled_store_times_out_and_is_retried() {
    let mut w = world(6, Faults::default(), 3);
    w.fill_tx(10, 0);
    w.server.script(Behavior::Stall);
    w.s3.store(&mut w.io, 1, "k", TX, 10, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 2
        }
    );
}

/// With resets, short writes and delays, every store either succeeds and is
/// then readable byte for byte, or ends in a reported fatal outcome.
#[test]
fn success_means_durable_under_faults() {
    let faults = Faults {
        reset: 80_000,
        short_send: 200_000,
        partial_recv: 200_000,
        delay: 200_000,
        ..Faults::default()
    };
    let (mut stored, mut gave_up, mut retried) = (0, 0, 0);
    for seed in 0..60 {
        let mut w = world(seed, faults, 6);
        w.fill_tx(BUF, seed as u8);
        w.s3.store(&mut w.io, 1, "k", TX, BUF, Checksum::None)
            .unwrap();
        let d = w.last();
        retried += (d.attempts > 1) as u32;
        match d.outcome {
            Outcome::Stored => {
                stored += 1;
                let sent = w.io.buf(TX).to_vec();
                assert_eq!(
                    w.server.object("/k"),
                    Some(&sent[..]),
                    "seed {seed}: reported Stored but the server holds something else"
                );
            }
            Outcome::Fatal(Fatal::RetriesExhausted) => gave_up += 1,
            o => panic!("seed {seed}: unexpected outcome {o:?}"),
        }
        assert_eq!(w.s3.in_progress(), 0);
    }
    assert!(
        stored >= 30 && retried >= 10,
        "stored {stored}, gave up {gave_up}, retried {retried}"
    );
}

/// Two answers S3 documents as worth retrying although their status looks final.
#[test]
fn conflict_and_request_timeout_answers_are_retried() {
    let mut w = world(25, Faults::default(), 3);
    w.fill_tx(1000, 1);
    w.server.script(Behavior::Status(409));
    w.s3.store_no_clobber(&mut w.io, 1, "a", TX, 1000, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 2
        }
    );
    // A plain store has no conditional write to conflict with: 409 stays final.
    w.server.script(Behavior::Status(409));
    w.s3.store(&mut w.io, 2, "b", TX, 1000, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::Status(409)),
            attempts: 1
        }
    );

    w.server.script(Behavior::Raw(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 42\r\n\r\n<Error><Code>RequestTimeout</Code></Error>"));
    w.s3.store(&mut w.io, 3, "c", TX, 1000, Checksum::None)
        .unwrap();
    // The scripted reply closes its connection, so the retry may first meet
    // the closed connection and need one more attempt.
    let d = w.last();
    assert!(d.outcome == Outcome::Stored && d.attempts > 1, "{d:?}");
    w.server.script(Behavior::Raw(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 39\r\n\r\n<Error><Code>InvalidArgs</Code></Error>"));
    w.s3.store(&mut w.io, 4, "d", TX, 1000, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 4,
            outcome: Outcome::Fatal(Fatal::Status(400)),
            attempts: 1
        }
    );
}
