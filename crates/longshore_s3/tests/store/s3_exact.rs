// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `put`, `head` and `get` against the in-tree HTTP server on one simulated
//! `Io`: what they report when the store answers, refuses, falls silent or
//! runs out of time, with heap allocation forbidden while they run.

mod common;
use common::*;
use longshore_s3::crypto::{hex, sha256};
use longshore_s3::multipart::Fixed;
use longshore_s3::report::{
    Answer, Cause, ContentRange, Encryption, End, Expect, Limit, Put, Report, ServerEncryption,
};

const ONCE: Limit = Limit {
    attempts: 1,
    deadline_ns: None,
};
const LEN: u32 = 1000;
/// SHA-256 of nothing, in hex, as FIPS 180-4's examples give it.
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn sha256_header(body: &[u8]) -> [u8; 44] {
    let mut out = [0u8; 44];
    longshore_crc::base64(&sha256(body), &mut out);
    out
}

/// A store that keeps versions and checks SHA-256 checksums.
fn strict_world(seed: u64) -> World {
    let mut w = world_with(seed, Faults::default(), 4, true);
    w.server.keep_versions();
    w.server.check_sha256(sha256_header);
    w
}

fn report(w: &mut World) -> Report {
    match w.last().outcome {
        Outcome::Report(report) => report,
        other => panic!("not a report: {other:?}"),
    }
}

fn answer(w: &mut World) -> Answer {
    match report(w).end {
        End::Answered(answer) => answer,
        other => panic!("not answered: {other:?}"),
    }
}

/// The options of an archive write: only if absent, encrypted, and with
/// the caller's SHA-256 of what is in the send buffer.
fn archive(w: &mut World) -> Put {
    Put {
        no_clobber: true,
        encryption: Some(Encryption::Aes256),
        checksum: Checksum::Sha256(sha256(&w.io.buf(TX)[..LEN as usize])),
    }
}

#[test]
fn a_write_is_read_back_under_the_identity_the_store_gave_it() {
    let mut w = strict_world(1);
    w.fill_tx(LEN, 3);
    let body = w.io.buf(TX)[..LEN as usize].to_vec();
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 1, "clips/a", TX, LEN, &options, ONCE)
        .unwrap();
    let put = report(&mut w);
    let End::Answered(stored) = put.end else {
        panic!("{put:?}")
    };
    assert_eq!((put.sent, put.unanswered), (1, 0));
    assert_eq!(stored.status, 200);
    assert_eq!(stored.encryption, Some(ServerEncryption::Aes256));
    assert_eq!(stored.sha256, Some(sha256(&body)));
    assert_eq!(stored.version, Fixed::new(b"sim-v1"));
    assert!(stored.request_id.is_some() && !stored.malformed);
    let expect = Expect {
        etag: stored.etag,
        version: stored.version,
    };
    assert!(expect.etag.is_some(), "the store names what it stored");

    w.s3.head(&mut w.io, 2, "clips/a", &expect, ONCE).unwrap();
    let head = answer(&mut w);
    assert_eq!((head.status, head.body_len), (200, 0));
    assert_eq!(head.content_length, Some(LEN as u64));
    assert_eq!((head.etag, head.version), (expect.etag, expect.version));
    assert_eq!(head.encryption, Some(ServerEncryption::Aes256));
    assert_eq!(head.sha256, Some(sha256(&body)));

    w.s3.get(&mut w.io, 3, "clips/a", None, &expect, RX, ONCE)
        .unwrap();
    let whole = answer(&mut w);
    assert_eq!((whole.status, whole.body_len), (200, LEN));
    assert_eq!(whole.etag, expect.etag);
    assert_eq!(&w.io.buf(RX)[..LEN as usize], &body[..]);

    w.s3.get(&mut w.io, 4, "clips/a", Some((100, 299)), &expect, RX, ONCE)
        .unwrap();
    let part = answer(&mut w);
    assert_eq!((part.status, part.body_len), (206, 200));
    assert_eq!(
        part.content_range,
        Some(ContentRange {
            first: 100,
            last: 299,
            size: Some(LEN as u64)
        })
    );
    assert_eq!((part.etag, part.version), (expect.etag, expect.version));
    assert_eq!(&w.io.buf(RX)[..200], &body[100..300]);
    assert_eq!(w.server.requests, 4, "one request each");
}

/// The canonical requests are written out here as the SigV4 specification
/// lays them out, not built with the client's own signing code.
#[test]
fn the_request_evidence_is_the_hash_of_the_canonical_request_that_was_sent() {
    let mut w = strict_world(2);
    w.fill_tx(LEN, 4);
    let options = archive(&mut w);
    let checksum = sha256_header(&w.io.buf(TX)[..LEN as usize]);
    w.s3.put(&mut w.io, 1, "clips/a", TX, LEN, &options, ONCE)
        .unwrap();
    let put = report(&mut w);
    let signed = put.request.expect("a request was sent");
    let End::Answered(stored) = put.end else {
        panic!("{put:?}")
    };
    let names = "host;if-none-match;x-amz-checksum-sha256;x-amz-content-sha256;x-amz-date;\
                 x-amz-server-side-encryption";
    assert_eq!(signed.method, "PUT");
    assert_eq!(signed.signed_headers.as_bytes(), names.as_bytes());
    let canonical = format!(
        "PUT\n/clips/a\n\nhost:bucket.objects.test\nif-none-match:*\n\
         x-amz-checksum-sha256:{}\nx-amz-content-sha256:UNSIGNED-PAYLOAD\nx-amz-date:{}\n\
         x-amz-server-side-encryption:AES256\n\n{names}\nUNSIGNED-PAYLOAD",
        core::str::from_utf8(&checksum).unwrap(),
        core::str::from_utf8(&signed.date).unwrap(),
    );
    assert_eq!(
        signed.canonical_request_sha256,
        sha256(canonical.as_bytes())
    );

    let expect = Expect {
        etag: stored.etag,
        version: stored.version,
    };
    w.s3.get(&mut w.io, 2, "clips/a", Some((100, 299)), &expect, RX, ONCE)
        .unwrap();
    let signed = report(&mut w).request.expect("a request was sent");
    let names = "host;if-match;range;x-amz-content-sha256;x-amz-date";
    assert_eq!(signed.method, "GET");
    assert_eq!(signed.signed_headers.as_bytes(), names.as_bytes());
    let canonical = format!(
        "GET\n/clips/a\nversionId=sim-v1\nhost:bucket.objects.test\nif-match:{}\n\
         range:bytes=100-299\nx-amz-content-sha256:{EMPTY}\nx-amz-date:{}\n\n{names}\n{EMPTY}",
        core::str::from_utf8(stored.etag.unwrap().as_bytes()).unwrap(),
        core::str::from_utf8(&signed.date).unwrap(),
    );
    assert_eq!(
        signed.canonical_request_sha256,
        sha256(canonical.as_bytes())
    );
    // Nothing of the credentials is in the evidence.
    let mut digest = [0u8; 64];
    hex(&signed.canonical_request_sha256, &mut digest);
    let shown = format!("{signed:?}{}", core::str::from_utf8(&digest).unwrap());
    assert!(!shown.contains("AKIDEXAMPLE") && !shown.contains("wJalrXUtnFEMI"));
}

#[test]
fn a_refused_write_reports_the_refusal_and_leaves_the_object_alone() {
    let mut w = strict_world(3);
    w.fill_tx(LEN, 5);
    let first = w.io.buf(TX)[..LEN as usize].to_vec();
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    assert_eq!(answer(&mut w).status, 200);

    w.fill_tx(LEN, 6);
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 2, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let refused = report(&mut w);
    let End::Answered(a) = refused.end else {
        panic!("{refused:?}")
    };
    assert_eq!(a.status, 412);
    // One request, answered: the object there is someone else's.
    assert_eq!((refused.sent, refused.unanswered), (1, 0));
    assert_eq!(w.server.object("/k"), Some(&first[..]));
    assert_eq!(w.server.requests, 2, "no request is made to compare");
}

#[test]
fn a_body_that_does_not_match_its_sha256_is_refused_by_the_store() {
    let mut w = strict_world(4);
    w.fill_tx(LEN, 7);
    let mut options = archive(&mut w);
    options.checksum = Checksum::Sha256(sha256(b"some other bytes"));
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let a = answer(&mut w);
    assert_eq!((a.status, a.etag), (400, None));
    assert_eq!(w.server.object("/k"), None);
}

#[test]
fn reads_of_another_object_or_version_are_refused_not_served() {
    let mut w = strict_world(5);
    w.fill_tx(LEN, 8);
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let stored = answer(&mut w);
    w.io.buf(RX)[..LEN as usize].fill(0xEE);

    let other_tag = Expect {
        etag: Fixed::new(b"\"0000000000000000\""),
        version: stored.version,
    };
    w.s3.head(&mut w.io, 2, "k", &other_tag, ONCE).unwrap();
    assert_eq!(answer(&mut w).status, 412);
    w.s3.get(&mut w.io, 3, "k", Some((0, 9)), &other_tag, RX, ONCE)
        .unwrap();
    let a = answer(&mut w);
    assert_eq!((a.status, a.body_len, a.content_range), (412, 0, None));

    let other_version = Expect {
        etag: stored.etag,
        version: Fixed::new(b"sim-v7"),
    };
    w.s3.get(&mut w.io, 4, "k", None, &other_version, RX, ONCE)
        .unwrap();
    let a = answer(&mut w);
    assert_eq!((a.status, a.body_len), (404, 0));
    assert!(
        w.io.buf(RX)[..LEN as usize].iter().all(|b| *b == 0xEE),
        "no byte of the object was delivered"
    );
}

#[test]
fn a_version_id_of_the_longest_length_s3_gives_is_sent_back() {
    let mut w = strict_world(13);
    w.fill_tx(LEN, 9);
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let stored = answer(&mut w);
    // S3 version ids run to 1024 bytes, and '/' and '+' are percent-encoded.
    let mut long = [b'a'; 1024];
    for (i, b) in long.iter_mut().enumerate().filter(|(i, _)| i % 2 == 0) {
        *b = if i % 4 == 0 { b'/' } else { b'+' };
    }
    let unknown = Expect {
        etag: stored.etag,
        version: Fixed::new(&long),
    };
    assert!(unknown.version.is_some());

    w.s3.head(&mut w.io, 2, "k", &unknown, ONCE).unwrap();
    let r = report(&mut w);
    assert_eq!((r.sent, r.unanswered), (1, 0));
    let End::Answered(a) = r.end else {
        panic!("{r:?}")
    };
    // The store read the id and had no such version of the object.
    assert_eq!(a.status, 404);
    w.s3.get(&mut w.io, 3, "k", None, &unknown, RX, ONCE)
        .unwrap();
    assert_eq!(answer(&mut w).status, 404);
    assert_eq!(w.server.requests, 3);
}

#[test]
fn an_entity_tag_longer_than_the_one_stored_does_not_match_it() {
    let mut w = strict_world(14);
    w.fill_tx(LEN, 10);
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let stored = answer(&mut w);
    w.io.buf(RX)[..LEN as usize].fill(0xEE);
    let mut tag = [b'7'; 200];
    (tag[0], tag[199]) = (b'"', b'"');
    let other_tag = Expect {
        etag: Fixed::new(&tag),
        version: stored.version,
    };
    assert!(other_tag.etag.is_some());

    w.s3.head(&mut w.io, 2, "k", &other_tag, ONCE).unwrap();
    assert_eq!(answer(&mut w).status, 412);
    w.s3.get(&mut w.io, 3, "k", None, &other_tag, RX, ONCE)
        .unwrap();
    let a = answer(&mut w);
    assert_eq!((a.status, a.body_len), (412, 0));
    assert!(w.io.buf(RX)[..LEN as usize].iter().all(|b| *b == 0xEE));
}

#[test]
fn a_range_the_store_cuts_short_is_reported_as_served() {
    let mut w = strict_world(6);
    w.fill_tx(LEN, 9);
    let options = archive(&mut w);
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    answer(&mut w);
    // Two hundred bytes are asked for where the object has one hundred left.
    w.s3.get(
        &mut w.io,
        2,
        "k",
        Some((900, 1099)),
        &Expect::default(),
        RX,
        ONCE,
    )
    .unwrap();
    let a = answer(&mut w);
    assert_eq!((a.status, a.body_len), (206, 100));
    assert_eq!(
        a.content_range,
        Some(ContentRange {
            first: 900,
            last: 999,
            size: Some(LEN as u64)
        })
    );
    assert_eq!(
        w.s3.get(
            &mut w.io,
            3,
            "k",
            Some((9, 3)),
            &Expect::default(),
            RX,
            ONCE
        ),
        Err(SubmitError::BadRange)
    );
}

#[test]
fn what_the_store_does_not_send_is_not_reported() {
    let mut w = world_with(7, Faults::default(), 4, true);
    w.server.ignore_encryption();
    w.fill_tx(LEN, 10);
    let options = Put {
        no_clobber: true,
        encryption: Some(Encryption::Aes256),
        checksum: Checksum::Compute,
    };
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let a = answer(&mut w);
    assert_eq!(a.status, 200);
    // Encryption was asked for and versions were not kept: the answer
    // carries neither, whatever was asked.
    assert_eq!((a.encryption, a.version, a.sha256), (None, None, None));
    assert!(!a.malformed);

    // A whole object stored with a CRC-64/NVME is checked against it.
    w.s3.get(&mut w.io, 2, "k", None, &Expect::default(), RX, ONCE)
        .unwrap();
    let a = answer(&mut w);
    assert_eq!((a.status, a.verified), (200, Some(true)));
    assert!(a.crc64.is_some());
}

#[test]
fn one_attempt_sends_one_request_whatever_the_store_does() {
    for (behavior, stored) in [
        (Behavior::CloseBeforeResponse, false),
        (Behavior::ApplyThenClose, true),
    ] {
        let mut w = strict_world(8);
        w.fill_tx(LEN, 11);
        let options = archive(&mut w);
        w.server.script(behavior);
        w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
            .unwrap();
        let r = report(&mut w);
        // Sent and not answered: the caller is told it cannot know, and
        // the store shows that it could have gone either way.
        assert!(
            matches!(r.end, End::Unknown(Cause::Transport(_))),
            "{behavior:?}: {r:?}"
        );
        assert_eq!((r.sent, r.unanswered), (1, 1), "{behavior:?}");
        assert!(r.request.is_some());
        assert_eq!(w.server.requests, 1, "{behavior:?}: nothing was retried");
        assert_eq!(w.server.object("/k").is_some(), stored, "{behavior:?}");
    }

    // A status the other operations would retry is reported as it is.
    let mut w = strict_world(9);
    w.fill_tx(LEN, 12);
    let options = archive(&mut w);
    w.server.script(Behavior::Status(503));
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, ONCE)
        .unwrap();
    let r = report(&mut w);
    assert!(
        matches!(r.end, End::Answered(a) if a.status == 503),
        "{r:?}"
    );
    assert_eq!((w.server.requests, w.server.object("/k")), (1, None));
}

#[test]
fn more_attempts_retry_and_do_not_hide_a_request_that_went_unanswered() {
    let twice = Limit {
        attempts: 2,
        deadline_ns: None,
    };
    let mut w = strict_world(10);
    w.fill_tx(LEN, 13);
    let options = archive(&mut w);
    w.server.script(Behavior::Status(503));
    w.s3.put(&mut w.io, 1, "a", TX, LEN, &options, twice)
        .unwrap();
    let r = report(&mut w);
    assert!(
        matches!(r.end, End::Answered(a) if a.status == 200),
        "{r:?}"
    );
    assert_eq!((r.sent, r.unanswered), (2, 0));

    // The first request is carried out and its answer lost. The second is
    // refused because the object exists. Nothing is compared or concluded:
    // the refusal is reported with the count of what went unanswered.
    w.server.script(Behavior::ApplyThenClose);
    w.s3.put(&mut w.io, 2, "b", TX, LEN, &options, twice)
        .unwrap();
    let r = report(&mut w);
    assert!(
        matches!(r.end, End::Answered(a) if a.status == 412),
        "{r:?}"
    );
    assert_eq!((r.sent, r.unanswered), (2, 1));
    assert_eq!(w.server.requests, 4);
}

#[test]
fn a_deadline_already_past_sends_nothing() {
    let mut w = strict_world(11);
    w.fill_tx(LEN, 14);
    let options = archive(&mut w);
    let past = Limit {
        attempts: 1,
        deadline_ns: Some(w.io.now_ns()),
    };
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, past)
        .unwrap();
    assert_eq!(
        report(&mut w),
        Report {
            end: End::NotSent(Cause::Deadline),
            request: None,
            sent: 0,
            unanswered: 0
        }
    );
    w.s3.head(&mut w.io, 2, "k", &Expect::default(), past)
        .unwrap();
    assert_eq!(report(&mut w).end, End::NotSent(Cause::Deadline));
    assert_eq!((w.server.requests, w.server.accepted), (0, 0));
}

#[test]
fn a_deadline_stops_a_request_the_store_never_answers() {
    let mut w = strict_world(12);
    w.fill_tx(LEN, 15);
    let options = archive(&mut w);
    let start = w.io.now_ns();
    let limit = Limit {
        attempts: 4,
        deadline_ns: Some(start + 300 * MS),
    };
    w.server.script(Behavior::Stall);
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, limit)
        .unwrap();
    let r = report(&mut w);
    // The request went out, so the store may have acted: the operation
    // ends unknown, at the deadline and not at the request timeout of a
    // second, and it does not try again although attempts remain.
    assert_eq!(r.end, End::Unknown(Cause::Deadline));
    assert_eq!((r.sent, r.unanswered), (1, 1));
    assert!(r.request.is_some());
    let took = w.io.now_ns() - start;
    assert!((300 * MS..400 * MS).contains(&took), "took {took} ns");
    assert_eq!(w.server.requests, 1);
    // The operation is over in the store as well: nothing of it is left
    // in progress, and its buffer carries the next operation.
    assert_eq!(w.s3.in_progress(), 0);
    w.s3.put(&mut w.io, 2, "k", TX, LEN, &options, ONCE)
        .unwrap();
    assert_eq!(answer(&mut w).status, 200);
}

#[test]
fn a_deadline_ends_a_wait_between_attempts() {
    let mut w = strict_world(13);
    w.fill_tx(LEN, 16);
    let options = archive(&mut w);
    let start = w.io.now_ns();
    // The store asks for a pause that is longer than the time left.
    let limit = Limit {
        attempts: 4,
        deadline_ns: Some(start + 50 * MS),
    };
    w.server.script(Behavior::Status(503));
    w.s3.put(&mut w.io, 1, "k", TX, LEN, &options, limit)
        .unwrap();
    let r = report(&mut w);
    // The one request sent was answered, and the next was never made: the
    // operation ends with that answer, not as unknown.
    assert!(
        matches!(r.end, End::Answered(a) if a.status == 503),
        "{r:?}"
    );
    assert_eq!((r.sent, r.unanswered), (1, 0));
    assert!(w.io.now_ns() - start < 100 * MS, "it ended at the deadline");
    assert_eq!((w.server.requests, w.server.object("/k")), (1, None));
}
