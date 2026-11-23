// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store against the in-tree HTTP server on one simulated `Io`, with
//! heap allocation forbidden while operations run.

#[cfg(test)]
mod common;
use common::*;

#[test]
fn store_fetch_range_and_erase() {
    let mut w = world(1, Faults::default(), 4);
    w.fill_tx(BUF, 5);
    w.s3.store(&mut w.io, 1, "dir/key one.bin", TX, BUF, Checksum::None)
        .unwrap();
    assert_eq!(w.last(), Done::new(1, Outcome::Stored, 1));
    // The key is percent-encoded on the wire; '/' is kept.
    assert_eq!(w.server.object_len("/dir/key%20one.bin"), Some(BUF));

    w.s3.fetch(&mut w.io, 2, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(
        w.last().outcome(),
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
        w.last().outcome(),
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
    assert_eq!(w.last().outcome(), Outcome::Erased);
    w.s3.fetch(&mut w.io, 5, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(w.last(), Done::new(5, Outcome::NotFound, 1));
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
    assert_eq!(w.last().outcome(), Outcome::Stored);
    let first = w.io.buf(TX)[..1000].to_vec();
    w.fill_tx(500, 2);
    w.s3.store_no_clobber(&mut w.io, 2, "k", TX, 500, Checksum::None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done::new(2, Outcome::AlreadyExists { after_retry: false }, 1)
    );
    assert_eq!(
        w.server.object("/k"),
        Some(&first[..]),
        "existing object must be unchanged"
    );
    // A plain store does replace it.
    w.s3.store(&mut w.io, 3, "k", TX, 500, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome(), Outcome::Stored);
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
    assert_eq!(w.last().outcome(), Outcome::Stored);
    w.server.script(Behavior::StatusWithBody(200, 1000));
    w.s3.fetch(&mut w.io, 2, "k", Some((100, 199)), RX).unwrap();
    assert_eq!(
        w.last(),
        Done::new(2, Outcome::Fatal(Fatal::RangeIgnored), 1)
    );
    assert_eq!(
        w.s3.fetch(&mut w.io, 3, "k", Some((200, 100)), RX),
        Err(SubmitError::BadRange),
        "a range that ends before it starts"
    );
    w.s3.fetch(&mut w.io, 4, "k", Some((100, 100)), RX).unwrap();
    assert_eq!(
        w.last().outcome(),
        Outcome::Fetched {
            len: 1,
            verified: false
        }
    );
}

/// Send `doc` as the body of a response with this status line.
fn raw(status: &str, doc: &str) -> Behavior {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{doc}",
        doc.len()
    );
    Behavior::Raw(Box::leak(response.into_bytes().into_boxed_slice()))
}

/// A 404 to an erase means the object is gone only when the store's error
/// code says so. A store that repeats the key in its error document must not
/// turn a missing bucket into a success because of what the key is called.
#[test]
fn an_erase_reads_the_error_code_and_not_the_key_echoed_beside_it() {
    let mut w = world(40, Faults::default(), 3);
    w.server.script(raw(
        "404 Not Found",
        "<Error><Code>NoSuchBucket</Code><Resource>/b/NoSuchKey.bin</Resource></Error>",
    ));
    w.s3.erase(&mut w.io, 1, "NoSuchKey.bin").unwrap();
    assert_eq!(
        w.last(),
        Done::new(1, Outcome::Fatal(Fatal::Status(404)), 1)
    );
    // The code itself still means the object is gone.
    w.server.script(raw(
        "404 Not Found",
        "<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>",
    ));
    w.s3.erase(&mut w.io, 2, "absent").unwrap();
    assert_eq!(w.last().outcome(), Outcome::Erased);
}

/// A key is refused by its length once encoded, at exactly the limit: one
/// byte of the path's room is kept back, so 511 encoded bytes are the most.
#[test]
fn a_key_is_accepted_up_to_the_encoded_limit_and_refused_one_byte_past_it() {
    use longshore_s3::s3::KEY_ENCODED_MAX;
    let mut w = world(41, Faults::default(), 1);
    let limit = KEY_ENCODED_MAX - 1;
    let mut token = 0;
    let mut submit = |key: &str| {
        token += 1;
        let result = w.s3.erase(&mut w.io, token, key);
        if result.is_ok() {
            // Whatever the test server makes of so long a key, the slot is freed.
            let _: Done = w.last();
        }
        result
    };
    // Bytes that stay as they are.
    assert_eq!(submit(&"a".repeat(limit)), Ok(()));
    assert_eq!(submit(&"a".repeat(limit + 1)), Err(SubmitError::BadKey));
    // Slashes are kept, not encoded.
    assert_eq!(submit(&"a/".repeat(limit / 2)), Ok(()));
    // Two bytes that become six: 85 of them are 510 bytes, 86 are 516.
    assert_eq!(submit(&"é".repeat(limit / 6)), Ok(()));
    assert_eq!(submit(&"é".repeat(limit / 6 + 1)), Err(SubmitError::BadKey));
    // One byte short of the limit, then a byte that needs three.
    assert_eq!(
        submit(&("a".repeat(limit - 1) + " ")),
        Err(SubmitError::BadKey)
    );
    assert_eq!(submit(&("a".repeat(limit - 3) + " ")), Ok(()));
    // A refused key takes no slot.
    assert_eq!(w.s3.in_progress(), 0);
}
