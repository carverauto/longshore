// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store against the in-tree HTTP server on one simulated `Io`, with
//! heap allocation forbidden while operations run.

mod common;
use common::*;

/// The first attempt creates the object but its response is lost. The retry
/// is refused as already existing; the client compares checksums and
/// reports the store as done.
#[test]
fn retried_no_clobber_store_recognises_its_own_object() {
    let mut w = world_with(11, Faults::default(), 4, true);
    w.fill_tx(1000, 1);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    // PUT (applied, response lost), PUT (412), HEAD (checksums agree).
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 3
        }
    );
    let sent = w.io.buf(TX)[..1000].to_vec();
    assert_eq!(w.server.object("/k"), Some(&sent[..]));
}

/// Someone else stored a different object at the key while our first attempt
/// was failing: the retry's refusal must not be mistaken for our own success.
#[test]
fn retried_no_clobber_store_rejects_a_different_object() {
    let mut w = world_with(12, Faults::default(), 4, true);
    w.fill_tx(1000, 7);
    w.s3.store(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    let theirs = w.io.buf(TX)[..1000].to_vec();

    w.fill_tx(1000, 8);
    w.server.script(Behavior::CloseBeforeResponse);
    w.s3.store_no_clobber(&mut w.io, 2, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::AlreadyExists { after_retry: false },
            attempts: 3
        }
    );
    assert_eq!(
        w.server.object("/k"),
        Some(&theirs[..]),
        "the other object must be unchanged"
    );
}

/// Without a checksum there is nothing to compare, and a store that keeps
/// no checksum cannot answer: both leave the decision to the caller.
#[test]
fn retried_no_clobber_store_without_a_checksum_is_undecided() {
    let mut w = world_with(13, Faults::default(), 4, true);
    w.fill_tx(1000, 1);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.store_no_clobber(&mut w.io, 1, "a", TX, 1000, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::AlreadyExists { after_retry: true },
            attempts: 2
        }
    );

    // This server keeps no checksums.
    let mut w = world_with(14, Faults::default(), 4, false);
    w.fill_tx(1000, 1);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.store_no_clobber(&mut w.io, 1, "a", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::AlreadyExists { after_retry: true },
            attempts: 3
        }
    );
}

/// Conditional stores under network faults, each key written once: every
/// one must end as stored, never as a false already-exists.
#[test]
fn no_clobber_stores_under_faults_are_never_falsely_refused() {
    let faults = Faults {
        reset: 30_000,
        short_send: 200_000,
        partial_recv: 200_000,
        delay: 200_000,
        ..Faults::default()
    };
    let keys = ["a", "b", "c", "d", "e", "f"];
    let mut comparisons = 0;
    for seed in 0..40 {
        let mut w = world_with(seed, faults, 8, true);
        for (i, key) in keys.iter().enumerate() {
            w.fill_tx(1000, i as u8);
            w.s3.store_no_clobber(&mut w.io, i as u64, key, TX, 1000, Checksum::Compute)
                .unwrap();
            let d = w.last();
            assert_eq!(d.outcome, Outcome::Stored, "seed {seed}, key {key}: {d:?}");
        }
        comparisons += w.s3.store_comparisons;
    }
    assert!(comparisons > 0, "no seed lost a response after the store applied it; the faults are too mild to test anything");
}

/// After a retried conditional store is refused, the comparison request can
/// itself fail. The refusal already proved an object exists and it may be
/// ours, so the caller must hear "already exists, undecided", never a plain
/// failure that hides it.
#[test]
fn a_failed_comparison_still_reports_that_an_object_exists() {
    // Refused: the credentials may write but not read.
    let mut w = world_with(22, Faults::default(), 4, true);
    w.fill_tx(1000, 1);
    for b in [
        Behavior::ApplyThenClose,
        Behavior::Normal,
        Behavior::Status(403),
    ] {
        w.server.script(b);
    }
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::AlreadyExists { after_retry: true },
            attempts: 3
        }
    );

    // Throttled on the last attempt the budget allows.
    let mut w = world_with(23, Faults::default(), 3, true);
    w.fill_tx(1000, 1);
    for b in [
        Behavior::ApplyThenClose,
        Behavior::Normal,
        Behavior::Status(503),
    ] {
        w.server.script(b);
    }
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::AlreadyExists { after_retry: true },
            attempts: 3
        }
    );

    // Throttled with attempts to spare: the comparison is simply tried again.
    let mut w = world_with(24, Faults::default(), 5, true);
    w.fill_tx(1000, 1);
    for b in [
        Behavior::ApplyThenClose,
        Behavior::Normal,
        Behavior::Status(503),
    ] {
        w.server.script(b);
    }
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 4
        }
    );
}
