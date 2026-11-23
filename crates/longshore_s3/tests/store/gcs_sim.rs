// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The Google Cloud Storage dialect in the simulator: bearer tokens,
//! CRC-32C in `x-goog-hash`, and `x-goog-if-generation-match: 0`.
//! Allocation is forbidden while operations run.

#[cfg(test)]
mod gcs_world;
use gcs_world::*;

#[global_allocator]
static ALLOC: Guard = Guard;

#[test]
fn store_fetch_list_and_erase_with_a_bearer_token_and_crc32c() {
    let mut w = world(1, Faults::default(), 3);
    w.fill_tx(5000, 1);
    w.s3.store(&mut w.io, 1, "dir/key one.bin", TX, 5000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
    let sent = w.io.buf(TX)[..5000].to_vec();
    assert_eq!(w.server.object("/dir/key%20one.bin"), Some(&sent[..]));

    w.s3.fetch(&mut w.io, 2, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 5000,
            verified: true
        },
        "the store's CRC-32C must be checked against the bytes received"
    );
    assert_eq!(&w.io.buf(RX)[..5000], &sent[..]);
    w.s3.fetch(&mut w.io, 3, "dir/key one.bin", Some((10, 19)), RX)
        .unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 10,
            verified: false
        }
    );

    w.s3.list(&mut w.io, 4, "dir/", None, 10, RX).unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("listing failed: {:?}", w.done.last())
    };
    let listing = longshore_s3::list::Listing::parse(&w.io.buf(RX)[..len as usize]).unwrap();
    assert_eq!(listing.entries().count(), 1);

    w.s3.erase(&mut w.io, 5, "dir/key one.bin").unwrap();
    assert_eq!(w.last().outcome, Outcome::Erased);
    w.s3.fetch(&mut w.io, 6, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::NotFound);
}

/// Google answers DELETE of an absent object with 404 NoSuchKey
/// (cloud.google.com/storage/docs/xml-api/delete-object): the object is gone,
/// so the erase succeeded. Any other 404 is still a failure.
#[test]
fn an_erase_of_an_absent_object_succeeds_and_other_404s_do_not() {
    let mut w = world(4, Faults::default(), 3);
    w.s3.erase(&mut w.io, 1, "never-stored").unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Erased,
            attempts: 1
        }
    );

    // The delete is applied and its response lost: the retry finds nothing.
    w.fill_tx(100, 1);
    w.s3.store(&mut w.io, 2, "k", TX, 100, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.erase(&mut w.io, 3, "k").unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 3,
            outcome: Outcome::Erased,
            attempts: 2
        }
    );
    assert_eq!(w.server.object("/k"), None);

    // A 404 that does not name the key (a missing bucket, say) stays fatal.
    w.server.script(Behavior::StatusWithBody(404, 64));
    w.s3.erase(&mut w.io, 4, "k").unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 4,
            outcome: Outcome::Fatal(Fatal::Status(404)),
            attempts: 1
        }
    );
}

#[test]
fn conditional_store_uses_the_generation_precondition() {
    let mut w = world(2, Faults::default(), 4);
    w.fill_tx(1000, 1);
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    let first = w.io.buf(TX)[..1000].to_vec();
    w.fill_tx(1000, 2);
    w.s3.store_no_clobber(&mut w.io, 2, "k", TX, 1000, Checksum::Compute)
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
        "the existing object must be unchanged"
    );

    // A response lost after the store applied it: the retry is refused, the
    // CRC-32C comparison recognises our own object.
    w.fill_tx(1000, 3);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.store_no_clobber(&mut w.io, 3, "other", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 3,
            outcome: Outcome::Stored,
            attempts: 3
        }
    );
}

#[test]
fn a_refused_token_is_fatal_and_a_new_token_is_rotated_in() {
    let mut w = world(3, Faults::default(), 3);
    w.fill_tx(100, 1);
    // The server stops accepting the token the client holds.
    w.server.allow_access_keys(&[TOKEN_B]);
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Fatal(Fatal::Status(403)),
            attempts: 1
        }
    );

    // The token helper writes a new token; it is validated and adopted.
    w.io.set_file(w.token_file, Some(format!("{TOKEN_B}\n").as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(w.s3.credential_rotations(), 1);
    w.s3.store(&mut w.io, 2, "k", TX, 100, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
}

/// Bits flipped in transit: a damaged upload is refused by the server's
/// CRC-32C check and sent again, a damaged download fails its check and is
/// fetched again. Nothing wrong is ever stored or returned.
#[test]
fn corruption_in_transit_never_reaches_the_store_or_the_caller() {
    let faults = Faults::NONE.with_bit_flip(150_000);
    let (mut stored, mut fetched) = (0, 0);
    for seed in 0..40 {
        let mut w = world(seed, faults, 6);
        w.fill_tx(8000, seed as u8);
        let sent = w.io.buf(TX)[..8000].to_vec();
        w.s3.store(&mut w.io, 1, "k", TX, 8000, Checksum::Compute)
            .unwrap();
        let d = w.last();
        // Anything but a success is a reported failure, never another success.
        assert!(
            matches!(d.outcome, Outcome::Stored | Outcome::Fatal(_)),
            "seed {seed}: {d:?}"
        );
        if d.outcome == Outcome::Stored {
            stored += 1;
            assert_eq!(
                w.server.object("/k"),
                Some(&sent[..]),
                "seed {seed}: a damaged body was stored"
            );
            w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
            let fetch = w.last();
            assert!(
                matches!(fetch.outcome, Outcome::Fetched { .. } | Outcome::Fatal(_)),
                "seed {seed}: {fetch:?}"
            );
            if let Outcome::Fetched { len, verified } = fetch.outcome {
                fetched += 1;
                assert!(
                    verified && len == 8000 && w.io.buf(RX)[..8000] == sent[..],
                    "seed {seed}: damaged bytes were returned"
                );
            }
        }
        assert_eq!(w.s3.in_progress(), 0);
    }
    assert!(
        stored > 20 && fetched > 10,
        "the faults leave too little to check: {stored} stored, {fetched} fetched"
    );
}

#[test]
fn a_multipart_upload_uses_the_same_forms_with_a_bearer_token() {
    use longshore_s3::multipart::{write_manifest, Part};
    let mut w = world(7, Faults::default(), 3);
    // Asking for whole-object checksums changes nothing here: this store
    // takes a CRC-32C with each part without being asked.
    w.s3.multipart_begin(&mut w.io, 1, "big", true).unwrap();
    let Outcome::Begun { upload } = w.last().outcome else {
        panic!("expected an upload")
    };
    let mut parts = Vec::new();
    let mut object = Vec::new();
    for number in 1..=2u32 {
        w.fill_tx(3000, number as u8);
        object.extend_from_slice(&w.io.buf(TX)[..3000]);
        w.s3.multipart_store_part(
            &mut w.io,
            10 + u64::from(number),
            "big",
            &upload,
            number,
            TX,
            3000,
            Checksum::Compute,
        )
        .unwrap();
        let Outcome::PartStored { tag, checksum } = w.last().outcome else {
            panic!("expected a stored part")
        };
        // The part went with a CRC-32C, which completion does not list.
        assert_eq!(checksum, None);
        parts.push(Part {
            number,
            tag,
            checksum: None,
        });
    }
    let len = write_manifest(w.io.buf(TX), &parts).unwrap() as u32;
    w.s3.multipart_complete(&mut w.io, 20, "big", &upload, TX, len, None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.s3.fetch(&mut w.io, 21, "big", None, RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 6000,
            verified: true
        }
    );
    assert!(w.io.buf(RX)[..6000] == object[..]);
}

#[test]
fn stat_options_and_copy_use_this_stores_header_names() {
    use longshore_s3::options::Options;
    use longshore_s3::stat::header_in;
    let mut w = world(8, Faults::default(), 3);
    w.fill_tx(3000, 4);
    let sent = w.io.buf(TX)[..3000].to_vec();
    let options = Options::new()
        .content_type("application/json")
        .and_then(|o| o.metadata("owner", "team a"))
        .unwrap();
    w.s3.store_with(&mut w.io, 1, "doc", TX, 3000, Checksum::Compute, &options)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);

    w.s3.copy(&mut w.io, 2, "doc", "copy").unwrap();
    assert_eq!(w.last().outcome, Outcome::Copied);
    w.s3.stat(&mut w.io, 3, "copy", Some(RX)).unwrap();
    let Outcome::Stat(stat) = w.last().outcome else {
        panic!("expected a stat")
    };
    assert_eq!(stat.size, 3000);
    // Eight base64 characters of CRC-32C, padded with zeros.
    assert_eq!(stat.checksum, Some(crc32c_form(&sent)));
    let head = &w.io.buf(RX)[..stat.head_len as usize];
    assert_eq!(header_in(head, "x-goog-meta-owner"), Some(&b"team a"[..]));
    assert_eq!(header_in(head, "x-amz-meta-owner"), None);
    assert_eq!(
        header_in(head, "content-type"),
        Some(&b"application/json"[..])
    );

    // This store has no batch erase, and a bearer token cannot sign a URL.
    assert_eq!(
        w.s3.erase_many(&mut w.io, 4, TX, 10, RX),
        Err(longshore_s3::s3::SubmitError::Unsupported)
    );
    let mut out = [0u8; 512];
    assert!(w
        .s3
        .presign(
            &mut w.io,
            &mut out,
            longshore_s3::presign::Access::Fetch,
            "doc",
            60
        )
        .is_err());
}
