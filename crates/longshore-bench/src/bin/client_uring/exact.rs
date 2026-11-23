// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `put`, `head` and `get` on real rings: a write that is read back under
//! the identity the store gave it, and a deadline that stops a request the
//! kernel is still working on.

use longshore::client::{Budget, Builder, Client};
use longshore::crypto::sha256;
use longshore::store::creds::Credentials;
use longshore::store::report::{
    Answer, Cause, ContentRange, Encryption, End, Expect, Put, Report, ServerEncryption,
};
use longshore::store::s3::Checksum;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

/// The value a body's `x-amz-checksum-sha256` header must have.
pub fn sha256_header(body: &[u8]) -> [u8; 44] {
    let mut out = [0u8; 44];
    longshore::checksum::base64(&sha256(body), &mut out);
    out
}

fn answered(report: &Result<Report, longshore::client::Error>, status: u16) -> Answer {
    match report.expect("a request was sent").end {
        End::Answered(answer) if answer.status == status => answer,
        other => panic!("expected an answer of {status}: {other:?}"),
    }
}

/// Write `len` bytes only if absent, with a SHA-256 and encryption asked
/// for, and read them back whole and in part as the object the store named.
pub fn round_trip(client: &Client, len: usize) {
    let key = "exact-object";
    let once = || Budget::once(Instant::now() + Duration::from_secs(30));
    let mut body = client.buffer_blocking();
    for (i, b) in body.as_mut_slice()[..len].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2246822519).to_le_bytes()[2];
    }
    let sent = body.as_slice()[..len].to_vec();
    let options = Put {
        no_clobber: true,
        encryption: Some(Encryption::Aes256),
        checksum: Checksum::Sha256(sha256(&sent)),
    };
    let (put, body) = client.put(key, body, len, &options, once()).wait();
    let report = put.expect("a request was sent");
    assert_eq!((report.sent, report.unanswered), (1, 0));
    assert_eq!(report.request.map(|signed| signed.method), Some("PUT"));
    let stored = answered(&Ok(report), 200);
    assert_eq!(stored.encryption, Some(ServerEncryption::Aes256));
    assert_eq!(stored.sha256, Some(sha256(&sent)));
    let expect = Expect {
        etag: stored.etag,
        version: stored.version,
    };
    assert!(expect.etag.is_some() && expect.version.is_some());

    let head = answered(&client.head(key, &expect, once()).wait(), 200);
    assert_eq!(head.content_length, Some(len as u64));
    assert_eq!((head.etag, head.version), (expect.etag, expect.version));

    let (whole, body) = client.get(key, None, &expect, body, once()).wait();
    assert_eq!(answered(&whole, 200).body_len as usize, len);
    assert!(body.as_slice()[..len] == sent[..], "bytes differ");
    let (part, body) = client
        .get(key, Some((4096, 8191)), &expect, body, once())
        .wait();
    let part = answered(&part, 206);
    let range = ContentRange {
        first: 4096,
        last: 8191,
        size: Some(len as u64),
    };
    assert_eq!((part.body_len, part.content_range), (4096, Some(range)));
    assert!(body.as_slice()[..4096] == sent[4096..8192], "bytes differ");

    // The key is taken now, and a body that is not what its SHA-256 says
    // is refused wherever it is written.
    let (again, body) = client.put(key, body, len, &options, once()).wait();
    answered(&again, 412);
    let wrong = Put {
        checksum: Checksum::Sha256(sha256(b"other bytes")),
        ..options
    };
    let (damaged, body) = client
        .put("exact-damaged", body, len, &wrong, once())
        .wait();
    answered(&damaged, 400);

    // A deadline that has passed sends nothing.
    let past = Budget::once(Instant::now() - Duration::from_millis(1));
    let (late, body) = client.put("exact-late", body, len, &options, past).wait();
    let late = late.expect("a report");
    assert_eq!((late.end, late.sent), (End::NotSent(Cause::Deadline), 0));
    assert!(late.request.is_none());
    let into = body;
    let (absent, _into) = client
        .get("exact-late", None, &Expect::default(), into, once())
        .wait();
    answered(&absent, 404);
    assert_eq!(client.erase(key).wait(), Ok(()));
}

/// A request to an address nothing answers ends at its deadline, and long
/// before the minute a request is otherwise given. The kernel is still
/// connecting when the deadline passes, so this cancels real work.
pub fn deadline_stops_a_connect() {
    // TEST-NET-1: routed nowhere, so the connect waits.
    let nowhere = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9);
    let credentials = Credentials::new("AKIDEXAMPLE", "secret").unwrap();
    let mut b = Builder::new("bucket.objects.test", "us-test-1", credentials);
    (b.port, b.address) = (nowhere.port(), Some(nowhere));
    (b.connections, b.buffers, b.operations) = (1, 1, 1);
    b.buffer_size = 64 * 1024;
    let client = b.build().expect("client");
    let body = client.buffer_blocking();
    let started = Instant::now();
    let budget = Budget {
        attempts: 4,
        deadline: Some(started + Duration::from_millis(300)),
    };
    let (put, _body) = client.put("k", body, 1024, &Put::default(), budget).wait();
    let took = started.elapsed();
    let report = put.expect("a report");
    // Nothing was sent either way. A host with no route at all refuses the
    // connect at once, and the attempts that follow are refused too until
    // the deadline ends them.
    assert!(
        matches!(
            report.end,
            End::NotSent(Cause::Deadline | Cause::Transport(_))
        ),
        "{report:?}"
    );
    assert_eq!((report.sent, report.request), (0, None));
    assert!(took < Duration::from_secs(5), "took {took:?}");
    println!(
        "ok exact: deadline ended an unanswered connect in {took:?} as {:?}",
        report.end
    );
}
