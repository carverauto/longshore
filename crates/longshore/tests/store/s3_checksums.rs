// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store against the in-tree HTTP server on one simulated `Io`, with
//! heap allocation forbidden while operations run.

mod common;
use common::*;

#[test]
fn checksums_are_sent_enforced_and_verified_on_fetch() {
    let mut w = world_with(20, Faults::default(), 3, true);
    w.fill_tx(BUF, 9);
    w.s3.store(&mut w.io, 1, "k", TX, BUF, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: BUF,
            verified: true
        }
    );
    // A ranged fetch cannot be checked against a whole-object checksum.
    w.s3.fetch(&mut w.io, 3, "k", Some((0, 9)), RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 10,
            verified: false
        }
    );

    // A caller-supplied checksum is used as given: the right one is accepted...
    let crc = longshore::checksum::crc64_nvme(&w.io.buf(TX)[..BUF as usize]);
    w.s3.store(&mut w.io, 4, "k2", TX, BUF, Checksum::Provided(crc))
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    // ...and a wrong one is indistinguishable from damage in transit, so it
    // is retried and then given up on. Nothing is stored.
    w.s3.store(&mut w.io, 5, "k3", TX, BUF, Checksum::Provided(crc ^ 1))
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 5,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: 3
        }
    );
    assert_eq!(w.server.object_len("/k3"), None);
}

/// Bits flipped in transit. With checksums on, a store that reports success
/// holds exactly the bytes sent, and a fetch that reports success returns
/// exactly the bytes stored. Damage is retried or reported, never accepted.
#[test]
fn bit_flips_never_become_silent_corruption_with_checksums_on() {
    let faults = Faults {
        bit_flip: 150_000,
        short_send: 200_000,
        partial_recv: 200_000,
        ..Faults::default()
    };
    let (mut stored, mut fetched, mut retried, mut refused) = (0, 0, 0, 0);
    for seed in 0..80 {
        let mut w = world_with(seed, faults, 8, true);
        w.fill_tx(BUF, seed as u8);
        let sent = w.io.buf(TX).to_vec();
        w.s3.store(&mut w.io, 1, "k", TX, BUF, Checksum::Compute)
            .unwrap();
        let d = w.last();
        retried += (d.attempts > 1) as u32;
        match d.outcome {
            Outcome::Stored => {
                stored += 1;
                assert_eq!(
                    w.server.object("/k"),
                    Some(&sent[..]),
                    "seed {seed}: Stored, but the server holds damaged bytes"
                );
            }
            Outcome::Fatal(_) => {
                refused += 1;
                continue;
            }
            o => panic!("seed {seed}: unexpected store outcome {o:?}"),
        }
        w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
        let d = w.last();
        retried += (d.attempts > 1) as u32;
        match d.outcome {
            Outcome::Fetched { len, verified } => {
                fetched += 1;
                assert!(
                    verified && len == BUF,
                    "seed {seed}: fetch succeeded unverified"
                );
                assert!(
                    w.io.buf(RX)[..] == sent[..],
                    "seed {seed}: Fetched, but the bytes are damaged"
                );
            }
            Outcome::Fatal(_) => refused += 1,
            o => panic!("seed {seed}: unexpected fetch outcome {o:?}"),
        }
    }
    // The run must actually exercise the damage paths to mean anything.
    assert!(
        stored >= 20 && fetched >= 15 && retried >= 15,
        "stored {stored}, fetched {fetched}, retried {retried}, refused {refused}"
    );
}

/// The same faults with checksums off do let damage through. This is what
/// the test above would look like if the checksum path did nothing.
#[test]
fn the_same_bit_flips_do_corrupt_data_with_checksums_off() {
    let faults = Faults {
        bit_flip: 150_000,
        short_send: 200_000,
        partial_recv: 200_000,
        ..Faults::default()
    };
    let mut damaged = 0;
    for seed in 0..80 {
        let mut w = world_with(seed, faults, 8, false);
        w.fill_tx(BUF, seed as u8);
        let sent = w.io.buf(TX).to_vec();
        w.s3.store(&mut w.io, 1, "k", TX, BUF, Checksum::None)
            .unwrap();
        if w.last().outcome == Outcome::Stored && w.server.object("/k") != Some(&sent[..]) {
            damaged += 1;
        }
    }
    assert!(
        damaged > 0,
        "expected unchecked stores to accept damaged bytes in at least one seed"
    );
}

#[test]
fn a_required_checksum_that_never_arrives_is_a_failure_not_an_unverified_success() {
    // The server stores objects without checksums (as Linode does), but the
    // client is configured for a store that always returns them.
    let mut w = world_with(21, Faults::default(), 3, true);
    w.fill_tx(100, 0);
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::ChecksumMismatch),
            attempts: 3
        }
    );
}
