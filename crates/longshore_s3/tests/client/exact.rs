// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `put`, `head` and `get` through the shareable client, over workers
//! driven by hand in the simulator.

#[cfg(test)]
#[path = "../store/common/mod.rs"]
mod common;
#[cfg(test)]
mod rig;
use common::{Behavior, Io};
use longshore_s3::client::{Budget, Error};
use longshore_s3::crypto::sha256;
use longshore_s3::report::{
    Cause, ContentRange, Encryption, End, Expect, Put, Report, ServerEncryption,
};
use longshore_s3::s3::Checksum;
use rig::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// One request, with far more time than the simulator needs.
fn once() -> Budget {
    Budget::once(Instant::now() + Duration::from_secs(3600))
}

fn archive(body: &[u8]) -> Put {
    Put {
        no_clobber: true,
        encryption: Some(Encryption::Aes256),
        checksum: Checksum::Sha256(sha256(body)),
    }
}

#[test]
fn a_write_and_its_reads_report_one_identity_through_lent_buffers() {
    let mut r = rig(1);
    r.lanes[0].server.keep_versions();
    let mut body = r.buffer();
    fill(&mut body, 5);
    let sent = body.as_slice()[..LEN].to_vec();
    let (put, body) = r.run(r.client.put("dir/a", body, LEN, &archive(&sent), once()));
    let put = put.expect("a request was sent");
    let End::Answered(stored) = put.end else {
        panic!("{put:?}")
    };
    assert_eq!((stored.status, put.sent, put.unanswered), (200, 1, 0));
    assert_eq!(stored.encryption, Some(ServerEncryption::Aes256));
    assert_eq!(stored.sha256, Some(sha256(&sent)));
    assert_eq!(put.request.map(|signed| signed.method), Some("PUT"));
    let expect = Expect {
        etag: stored.etag,
        version: stored.version,
    };
    assert!(expect.etag.is_some() && expect.version.is_some());

    let head = r.run(r.client.head("dir/a", &expect, once())).unwrap();
    let End::Answered(head) = head.end else {
        panic!("{head:?}")
    };
    assert_eq!((head.status, head.content_length), (200, Some(LEN as u64)));
    assert_eq!((head.etag, head.version), (expect.etag, expect.version));

    let (ranged, body) = r.run(r.client.get("dir/a", Some((10, 19)), &expect, body, once()));
    let End::Answered(ranged) = ranged.unwrap().end else {
        panic!("not answered")
    };
    assert_eq!((ranged.status, ranged.body_len), (206, 10));
    assert_eq!(
        ranged.content_range,
        Some(ContentRange {
            first: 10,
            last: 19,
            size: Some(LEN as u64)
        })
    );
    assert_eq!(&body.as_slice()[..10], &sent[10..20]);

    let (whole, body) = r.run(r.client.get("dir/a", None, &expect, body, once()));
    let End::Answered(whole) = whole.unwrap().end else {
        panic!("not answered")
    };
    assert_eq!((whole.status, whole.body_len), (200, LEN as u32));
    assert_eq!(&body.as_slice()[..LEN], &sent[..]);

    // A second write to the key is refused, and says so.
    let (again, _body) = r.run(r.client.put("dir/a", body, LEN, &archive(&sent), once()));
    assert!(matches!(again.unwrap().end, End::Answered(a) if a.status == 412));
}

#[test]
fn a_deadline_that_has_passed_sends_nothing_and_returns_the_buffer() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 6);
    let past = Budget::once(Instant::now() - Duration::from_millis(1));
    let (put, body) = r.run(r.client.put("k", body, LEN, &Put::default(), past));
    assert_eq!(
        put,
        Ok(Report {
            end: End::NotSent(Cause::Deadline),
            request: None,
            sent: 0,
            unanswered: 0
        })
    );
    let head = r.run(r.client.head("k", &Expect::default(), past));
    assert!(matches!(head, Ok(report) if report.end == End::NotSent(Cause::Deadline)));
    assert_eq!(r.lanes[0].server.requests(), 0);
    // The buffer is the caller's again and carries the next operation.
    let (next, _body) = r.run(r.client.put("k", body, LEN, &Put::default(), once()));
    assert!(matches!(next.unwrap().end, End::Answered(a) if a.status == 200));
}

#[test]
fn an_error_means_nothing_was_sent() {
    let mut r = rig(1);
    let body = r.buffer();
    let size = body.len();
    let (too_long, body) = r.run(r.client.put("k", body, size + 1, &Put::default(), once()));
    assert_eq!(too_long, Err(Error::TooLong));
    let (no_key, body) = r.run(r.client.put("", body, 1, &Put::default(), once()));
    assert_eq!(no_key, Err(Error::BadKey));
    let (bad_range, _body) =
        r.run(
            r.client
                .get("k", Some((9, 3)), &Expect::default(), body, once()),
        );
    assert_eq!(bad_range, Err(Error::BadRange));
    assert_eq!(
        r.run(r.client.head("", &Expect::default(), once())),
        Err(Error::BadKey)
    );
    assert_eq!(r.lanes[0].server.requests(), 0);
}

/// The result of a request the store never answers comes when the deadline
/// has stopped it, and not before: until then the buffer is not the
/// caller's and the operation holds its slot.
#[test]
fn a_stalled_request_ends_at_its_deadline_with_its_buffer() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 7);
    r.lanes[0].server.script(Behavior::Stall);
    let budget = Budget {
        attempts: 3,
        deadline: Some(Instant::now() + Duration::from_millis(200)),
    };
    let started = r.lanes[0].io.now_ns();
    let (put, body) = r.run(r.client.put("k", body, LEN, &Put::default(), budget));
    let put = put.unwrap();
    assert_eq!(put.end, End::Unknown(Cause::Deadline));
    assert_eq!((put.sent, put.unanswered), (1, 1));
    assert!(put.request.is_some());
    // On the simulator's clock: after the deadline, and long before the
    // request's own timeout of a second.
    let took = r.lanes[0].io.now_ns() - started;
    assert!(
        (150 * common::MS..400 * common::MS).contains(&took),
        "{took}"
    );
    assert_eq!(r.lanes[0].server.requests(), 1, "no second attempt");
    let (next, _body) = r.run(r.client.put("k2", body, LEN, &Put::default(), once()));
    assert!(matches!(next.unwrap().end, End::Answered(a) if a.status == 200));
}

/// An I/O thread that fails under a request already sent cannot say the
/// request was not carried out, and does not.
#[test]
fn a_failed_io_thread_reports_a_sent_request_as_unknown() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 8);
    r.lanes[0].server.script(Behavior::Stall);
    let client = r.client.clone();
    let mut put = pin!(client.put("k", body, LEN, &Put::default(), once()));
    let mut cx = Context::from_waker(Waker::noop());
    while r.lanes[0].server.requests() == 0 {
        assert!(put.as_mut().poll(&mut cx).is_pending());
        r.pump();
    }
    r.lanes[0].worker.abandon(true);
    let Poll::Ready((report, _body)) = put.as_mut().poll(&mut cx) else {
        panic!("the operation did not end")
    };
    let report = report.expect("a request was sent");
    assert_eq!(report.end, End::Unknown(Cause::Closed));
    assert_eq!((report.sent, report.unanswered), (1, 1));
    assert!(report.request.is_some());
}
