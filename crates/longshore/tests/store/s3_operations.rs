// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store against the in-tree HTTP server on one simulated `Io`, with
//! heap allocation forbidden while operations run.

mod common;
use common::*;

#[test]
fn store_fetch_range_and_erase() {
    let mut w = world(1, Faults::default(), 4);
    w.fill_tx(BUF, 5);
    w.s3.store(&mut w.io, 1, "dir/key one.bin", TX, BUF, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
    // The key is percent-encoded on the wire; '/' is kept.
    assert_eq!(w.server.object_len("/dir/key%20one.bin"), Some(BUF));

    w.s3.fetch(&mut w.io, 2, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: BUF,
            verified: false
        }
    );
    let sent = w.io.buf(TX).to_vec();
    assert_eq!(&w.io.buf(RX)[..], &sent[..]);

    w.s3.fetch(&mut w.io, 3, "dir/key one.bin", Some((100, 199)), RX)
        .unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 100,
            verified: false
        }
    );
    assert_eq!(
        &w.io.buf(RX)[..100],
        &sent[100..200],
        "ranged fetch returns exactly the requested bytes"
    );

    w.s3.erase(&mut w.io, 4, "dir/key one.bin").unwrap();
    assert_eq!(w.last().outcome, Outcome::Erased);
    w.s3.fetch(&mut w.io, 5, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 5,
            outcome: Outcome::NotFound,
            attempts: 1
        }
    );
    assert_eq!(
        w.s3.connects(),
        1,
        "all five operations reused one connection"
    );
}

#[test]
fn store_no_clobber_keeps_the_existing_object() {
    let mut w = world(2, Faults::default(), 4);
    w.fill_tx(1000, 1);
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    let first = w.io.buf(TX)[..1000].to_vec();
    w.fill_tx(500, 2);
    w.s3.store_no_clobber(&mut w.io, 2, "k", TX, 500, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::AlreadyExists { after_retry: false },
            attempts: 1
        }
    );
    assert_eq!(
        w.server.object("/k"),
        Some(&first[..]),
        "existing object must be unchanged"
    );
    // A plain store does replace it.
    w.s3.store(&mut w.io, 3, "k", TX, 500, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    assert_eq!(w.server.object_len("/k"), Some(500));
}

#[test]
fn submit_limits_and_bad_keys() {
    let mut w = world(7, Faults::default(), 3);
    assert_eq!(w.s3.erase(&mut w.io, 0, ""), Err(SubmitError::BadKey));
    assert_eq!(
        w.s3.erase(&mut w.io, 0, &"é".repeat(200)),
        Err(SubmitError::BadKey),
        "too long once encoded"
    );
    for t in 0..4 {
        w.server.script(Behavior::Stall);
        w.s3.erase(&mut w.io, t, "k").unwrap();
    }
    assert_eq!(w.s3.erase(&mut w.io, 9, "k"), Err(SubmitError::Busy));
    w.run_until_done(4);
    assert_eq!(w.s3.in_progress(), 0);
    assert_eq!(w.s3.erase(&mut w.io, 10, "k"), Ok(()));
}

/// A server may ignore Range and answer with the whole object. Handing
/// those bytes back as the range would be silent wrong data.
#[test]
fn a_ranged_fetch_answered_with_the_whole_object_is_refused() {
    let mut w = world(21, Faults::default(), 3);
    w.fill_tx(1000, 1);
    w.s3.store(&mut w.io, 1, "k", TX, 1000, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.server.script(Behavior::StatusWithBody(200, 1000));
    w.s3.fetch(&mut w.io, 2, "k", Some((100, 199)), RX).unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::RangeIgnored),
            attempts: 1
        }
    );
    assert_eq!(
        w.s3.fetch(&mut w.io, 3, "k", Some((200, 100)), RX),
        Err(SubmitError::BadRange),
        "a range that ends before it starts"
    );
    w.s3.fetch(&mut w.io, 4, "k", Some((100, 100)), RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 1,
            verified: false
        }
    );
}
