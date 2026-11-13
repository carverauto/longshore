// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Operations that work on objects without moving their bytes through the
//! caller: stat, optional headers, copy, batch erase, delimiter listing and
//! presigned URLs. Against the in-tree HTTP server on one simulated `Io`,
//! with heap allocation forbidden while operations run.

mod common;
use common::*;
use longshore::store::erase::{write_manifest, Failure, Failures};
use longshore::store::options::Options;
use longshore::store::presign::{Access, PresignError};
use longshore::store::stat::{header_in, Stat};

const LEN: u32 = 5000;

fn described() -> Options {
    Options::new()
        .content_type("text/plain")
        .and_then(|o| o.metadata("Owner", "team a"))
        .and_then(|o| o.storage_class("STANDARD_IA"))
        .unwrap()
}

impl World {
    fn stat(&mut self, token: u64, key: &str, into: Option<BufId>) -> Outcome {
        self.s3.stat(&mut self.io, token, key, into).unwrap();
        self.last().outcome
    }

    fn put(&mut self, token: u64, key: &str, salt: u8) {
        self.fill_tx(LEN, salt);
        self.s3
            .store(&mut self.io, token, key, TX, LEN, Checksum::Compute)
            .unwrap();
        assert_eq!(self.last().outcome, Outcome::Stored);
    }
}

#[test]
fn stat_reports_an_object_without_fetching_it() {
    let mut w = world_with(1, Faults::default(), 3, true);
    w.fill_tx(LEN, 3);
    let crc = longshore::checksum::crc64_nvme(&w.io.buf(TX)[..LEN as usize]);
    w.s3.store_with(
        &mut w.io,
        1,
        "doc",
        TX,
        LEN,
        Checksum::Compute,
        &described(),
    )
    .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);

    w.io.buf(RX).fill(0);
    let Outcome::Stat(stat) = w.stat(2, "doc", Some(RX)) else {
        panic!("expected a stat")
    };
    assert_eq!(stat.size, LEN as u64);
    assert_eq!(stat.modified, Some(1_445_412_480));
    assert_eq!(
        stat.checksum,
        Some(longshore::checksum::crc64_header_value(crc))
    );
    assert!(stat.tag.is_some_and(|t| t.as_bytes().starts_with(b"\"")));
    // No body was received: the buffer holds the head and nothing after it.
    let head = &w.io.buf(RX)[..stat.head_len as usize];
    assert!(head.starts_with(b"HTTP/1.1 200") && head.ends_with(b"\r\n\r\n"));
    assert_eq!(header_in(head, "content-type"), Some(&b"text/plain"[..]));
    assert_eq!(header_in(head, "x-amz-meta-owner"), Some(&b"team a"[..]));
    assert_eq!(
        header_in(head, "x-amz-storage-class"),
        Some(&b"STANDARD_IA"[..])
    );
    assert!(w.io.buf(RX)[stat.head_len as usize..]
        .iter()
        .all(|b| *b == 0));

    // Without a buffer the fields are the same and no head is copied.
    assert_eq!(
        w.stat(3, "doc", None),
        Outcome::Stat(Stat {
            head_len: 0,
            ..stat
        })
    );
    assert_eq!(w.stat(4, "absent", None), Outcome::NotFound);
}

#[test]
fn optional_headers_are_sent_again_on_a_retry() {
    let mut w = world(2, Faults::default(), 3);
    w.fill_tx(LEN, 1);
    w.server.script(Behavior::Status(503));
    w.server.script(Behavior::CloseBeforeResponse);
    w.s3.store_no_clobber_with(&mut w.io, 1, "k", TX, LEN, Checksum::None, &described())
        .unwrap();
    let d = w.last();
    assert_eq!((d.outcome, d.attempts), (Outcome::Stored, 3));
    let Outcome::Stat(stat) = w.stat(2, "k", Some(RX)) else {
        panic!("expected a stat")
    };
    let head = &w.io.buf(RX)[..stat.head_len as usize];
    assert_eq!(header_in(head, "x-amz-meta-owner"), Some(&b"team a"[..]));
    // A plain store of the same key afterwards carries none of them.
    w.put(3, "k", 2);
    let Outcome::Stat(stat) = w.stat(4, "k", Some(RX)) else {
        panic!("expected a stat")
    };
    let head = &w.io.buf(RX)[..stat.head_len as usize];
    assert_eq!(header_in(head, "x-amz-meta-owner"), None);
    assert_eq!(header_in(head, "content-type"), None);
}

#[test]
fn a_copy_is_made_inside_the_store() {
    let mut w = world_with(3, Faults::default(), 3, true);
    w.fill_tx(LEN, 7);
    let sent = w.io.buf(TX)[..LEN as usize].to_vec();
    w.s3.store_with(
        &mut w.io,
        1,
        "a/src",
        TX,
        LEN,
        Checksum::Compute,
        &described(),
    )
    .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);

    w.s3.copy(&mut w.io, 2, "a/src", "b/dst").unwrap();
    let d = w.last();
    assert_eq!((d.outcome, d.attempts), (Outcome::Copied, 1));
    w.s3.fetch(&mut w.io, 3, "b/dst", None, RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: LEN,
            verified: true
        }
    );
    assert!(w.io.buf(RX)[..LEN as usize] == sent[..]);
    // What describes the object came along.
    let Outcome::Stat(stat) = w.stat(4, "b/dst", Some(RX)) else {
        panic!("expected a stat")
    };
    let head = &w.io.buf(RX)[..stat.head_len as usize];
    assert_eq!(header_in(head, "content-type"), Some(&b"text/plain"[..]));

    w.s3.copy(&mut w.io, 5, "absent", "c").unwrap();
    assert_eq!(w.last().outcome, Outcome::NotFound);
    assert_eq!(w.s3.copy(&mut w.io, 6, "", "c"), Err(SubmitError::BadKey));
}

#[test]
fn a_copy_answered_with_an_error_document_is_not_a_copy() {
    let mut w = world(4, Faults::default(), 3);
    w.put(1, "src", 1);
    // Status 200 and a transient error: tried again.
    w.server
        .script(Behavior::ErrorDocument(200, "InternalError"));
    w.s3.copy(&mut w.io, 2, "src", "dst").unwrap();
    let d = w.last();
    assert_eq!((d.outcome, d.attempts), (Outcome::Copied, 2));
    assert!(w.server.object("/dst").is_some());
    // Status 200 and an error that will not improve.
    w.server
        .script(Behavior::ErrorDocument(200, "AccessDenied"));
    w.s3.copy(&mut w.io, 3, "src", "other").unwrap();
    let d = w.last();
    assert_eq!(
        (d.outcome, d.attempts),
        (Outcome::Fatal(Fatal::ErrorDocument), 1)
    );
    assert!(w.server.object("/other").is_none());
    // The response to a copy is lost: the retry copies again, harmlessly.
    w.server.script(Behavior::ApplyThenClose);
    w.s3.copy(&mut w.io, 4, "src", "third").unwrap();
    let d = w.last();
    assert_eq!((d.outcome, d.attempts), (Outcome::Copied, 2));
}

#[test]
fn a_batch_erase_reports_the_keys_it_could_not_erase() {
    let mut w = world_with(5, Faults::default(), 3, true);
    for (token, key) in [(1, "a"), (2, "dir/b&c"), (3, "locked/c")] {
        w.put(token, key, token as u8);
    }
    w.server.deny_erase("locked/");
    let keys = ["a", "dir/b&c", "locked/c", "never-there"];
    let len = write_manifest(w.io.buf(TX), &keys).unwrap() as u32;
    // One failed attempt first: the same document is sent again.
    w.server.script(Behavior::Status(503));
    w.s3.erase_many(&mut w.io, 10, TX, len, RX).unwrap();
    let d = w.last();
    let Outcome::ErasedMany { len } = d.outcome else {
        panic!("expected a batch result, got {d:?}")
    };
    assert_eq!(d.attempts, 2);
    let failures: Vec<_> = Failures::parse(&w.io.buf(RX)[..len as usize])
        .unwrap()
        .map(|f| f.unwrap())
        .collect();
    assert_eq!(
        failures,
        [Failure {
            key_raw: b"locked/c",
            code: b"AccessDenied"
        }]
    );
    assert_eq!(w.stat(11, "a", None), Outcome::NotFound);
    assert_eq!(w.stat(12, "dir/b&c", None), Outcome::NotFound);
    assert!(matches!(w.stat(13, "locked/c", None), Outcome::Stat(_)));
}

#[test]
fn a_delimiter_lists_one_level() {
    let mut w = world(6, Faults::default(), 3);
    for (token, key) in [(1, "a/1"), (2, "a/2"), (3, "b/1"), (4, "top")] {
        w.put(token, key, token as u8);
    }
    let list = |w: &mut World, prefix: &str, after: Option<&str>, max: u16| {
        w.s3.list_with(&mut w.io, 20, prefix, after, max, Some("/"), RX)
            .unwrap();
        let Outcome::Listed { len } = w.last().outcome else {
            panic!("listing failed")
        };
        let listing = Listing::parse(&w.io.buf(RX)[..len as usize]).unwrap();
        let keys: Vec<Vec<u8>> = listing
            .entries()
            .map(|e| e.unwrap().key_raw.to_vec())
            .collect();
        let prefixes: Vec<Vec<u8>> = listing
            .common_prefixes()
            .map(|p| p.unwrap().to_vec())
            .collect();
        (keys, prefixes, listing.is_truncated)
    };
    let (keys, prefixes, truncated) = list(&mut w, "", None, 100);
    assert_eq!(keys, [b"top"]);
    assert_eq!(prefixes, [b"a/", b"b/"]);
    assert!(!truncated);

    // Below a prefix the keys themselves are listed.
    let (keys, prefixes, _) = list(&mut w, "a/", None, 100);
    assert_eq!(keys, [b"a/1", b"a/2"]);
    assert!(prefixes.is_empty());

    // Paging past a common prefix, as the documentation says to.
    let (keys, prefixes, truncated) = list(&mut w, "", None, 1);
    assert!(keys.is_empty() && truncated);
    assert_eq!(prefixes, [b"a/"]);
    let (keys, prefixes, truncated) = list(&mut w, "", Some("a/\u{10FFFF}"), 1);
    assert!(keys.is_empty() && truncated);
    assert_eq!(prefixes, [b"b/"]);
    let (keys, prefixes, truncated) = list(&mut w, "", Some("b/\u{10FFFF}"), 1);
    assert_eq!(keys, [b"top"]);
    assert!(prefixes.is_empty() && !truncated);

    // The plain listing is unchanged: every key, no prefixes.
    w.s3.list(&mut w.io, 30, "", None, 100, RX).unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("listing failed")
    };
    let listing = Listing::parse(&w.io.buf(RX)[..len as usize]).unwrap();
    assert_eq!(listing.entries().count(), 4);
    assert_eq!(listing.common_prefixes().count(), 0);
}

#[test]
fn a_presigned_url_names_the_bucket_host_and_the_key() {
    let mut w = world(7, Faults::default(), 3);
    let mut out = [0u8; 1024];
    let n =
        w.s3.presign(&mut w.io, &mut out, Access::Fetch, "dir/a b", 3600)
            .unwrap();
    let url = core::str::from_utf8(&out[..n]).unwrap();
    assert!(url.starts_with("http://bucket.objects.test/dir/a%20b?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F"));
    assert!(url.contains("%2Fus-test-1%2Fs3%2Faws4_request&"));
    assert!(url.contains("&X-Amz-Expires=3600&X-Amz-SignedHeaders=host&X-Amz-Signature="));
    assert_eq!(
        w.s3.presign(&mut w.io, &mut out, Access::Store, "k", 0),
        Err(PresignError::BadExpiry)
    );
}
