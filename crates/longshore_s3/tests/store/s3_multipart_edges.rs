// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart upload where the store's answer is not the expected one: a
//! lost completion that cannot be confirmed, and success responses that
//! lack what they must carry. Against the in-tree HTTP server on one
//! simulated `Io`, with heap allocation forbidden while operations run.

mod common;
use common::parts::*;
use common::*;
use longshore_s3::multipart::{PartTag, UploadId, PART_TAG_MAX};

/// Send exactly this as the next response.
fn raw(response: String) -> Behavior {
    Behavior::Raw(Box::leak(response.into_bytes().into_boxed_slice()))
}

fn bad_response(token: u64) -> Done {
    Done {
        token,
        outcome: Outcome::Fatal(Fatal::BadResponse),
        attempts: 1,
    }
}

/// The completion's response is lost and the retry finds the upload gone,
/// but the object at the key is someone else's: the upload was aborted, not
/// completed. The checksums differ, so this is not reported as stored.
#[test]
fn a_lost_completion_is_not_confirmed_by_someone_elses_object() {
    let mut w = world_with(7, Faults::default(), 4, true);
    w.fill_tx(1000, 9);
    let theirs = w.io.buf(TX)[..1000].to_vec();
    w.s3.store(&mut w.io, 50, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);

    let upload = w.begin(1, "k", true);
    let (len, object) = w.three_parts("k", &upload);
    let crc = longshore_crc::crc64_nvme(&object);
    w.s3.multipart_abort(&mut w.io, 51, "k", &upload).unwrap();
    assert_eq!(w.last().outcome, Outcome::Aborted);

    // POST (lost), POST (404: no such upload), HEAD (another checksum).
    w.server.script(Behavior::CloseBeforeResponse);
    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, Some(crc))
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 20,
            outcome: Outcome::Fatal(Fatal::Status(404)),
            attempts: 3
        }
    );
    assert_eq!(w.s3.store_comparisons, 1);
    assert_eq!(w.server.object("/k"), Some(&theirs[..]));
}

/// The comparison after a lost completion is itself retried while attempts
/// remain.
#[test]
fn the_comparison_after_a_lost_completion_is_retried() {
    let mut w = world_with(8, Faults::default(), 4, true);
    let upload = w.begin(1, "k", true);
    let (len, object) = w.three_parts("k", &upload);
    let crc = longshore_crc::crc64_nvme(&object);
    // POST (applied, lost), POST (404), HEAD (503), HEAD (the checksums agree).
    for b in [
        Behavior::ApplyThenClose,
        Behavior::Normal,
        Behavior::Status(503),
    ] {
        w.server.script(b);
    }
    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, Some(crc))
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 20,
            outcome: Outcome::Stored,
            attempts: 4
        }
    );
    assert_eq!(w.server.object("/k"), Some(&object[..]));
}

/// With no attempt left for the comparison, nobody knows whether the
/// upload completed: the first attempt may have done it, as here. The
/// caller is told the attempts ran out, not that the upload does not exist,
/// which is what a 404 would say and is not known.
#[test]
fn a_comparison_that_runs_out_of_attempts_says_so_and_not_that_the_upload_is_gone() {
    let mut w = world_with(9, Faults::default(), 3, true);
    let upload = w.begin(1, "k", true);
    let (len, object) = w.three_parts("k", &upload);
    let crc = longshore_crc::crc64_nvme(&object);
    for b in [
        Behavior::ApplyThenClose,
        Behavior::Normal,
        Behavior::Status(503),
    ] {
        w.server.script(b);
    }
    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, Some(crc))
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 20,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: 3
        }
    );
    assert_eq!(w.server.object("/k"), Some(&object[..]));
}

/// A stored part is known by its tag. A success without one, or with one
/// longer than the type holds, is an error and never a part with a cut tag.
#[test]
fn a_part_stored_without_a_usable_tag_is_a_bad_response() {
    let mut w = world(10, Faults::default(), 3);
    let upload = w.begin(1, "k", false);
    let with_tag = |tag: &str| {
        raw(format!(
            "HTTP/1.1 200 OK\r\nETag: {tag}\r\nContent-Length: 0\r\n\r\n"
        ))
    };

    w.server.script(raw(
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_string()
    ));
    w.send_part(2, "k", &upload, 1, TX);
    assert_eq!(w.last(), bad_response(2));

    w.server.script(with_tag(&"t".repeat(PART_TAG_MAX + 1)));
    w.send_part(3, "k", &upload, 1, TX);
    assert_eq!(w.last(), bad_response(3));

    // The longest tag that fits is kept whole.
    let longest = "t".repeat(PART_TAG_MAX);
    w.server.script(with_tag(&longest));
    w.send_part(4, "k", &upload, 1, TX);
    let crc = longshore_crc::crc64_nvme(&part_bytes(1));
    assert_eq!(
        w.last(),
        Done {
            token: 4,
            outcome: Outcome::PartStored {
                tag: PartTag::new(longest.as_bytes()).unwrap(),
                checksum: Some(crc)
            },
            attempts: 1
        }
    );

    // Any other success status says nothing about a tag.
    w.server
        .script(raw("HTTP/1.1 204 No Content\r\n\r\n".to_string()));
    w.send_part(5, "k", &upload, 1, TX);
    assert_eq!(w.last(), bad_response(5));
}

#[test]
fn an_upload_begun_without_a_usable_id_is_a_bad_response() {
    let mut w = world(11, Faults::default(), 3);
    let with_body = |body: &str| {
        raw(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
    };
    for (token, body) in [
        (1, String::new()),
        (
            2,
            "<InitiateMultipartUploadResult><UploadId></UploadId></InitiateMultipartUploadResult>"
                .to_string(),
        ),
        (
            3,
            "<InitiateMultipartUploadResult><Key>k</Key></InitiateMultipartUploadResult>"
                .to_string(),
        ),
        (4, "<UploadId>cut off".to_string()),
    ] {
        w.server.script(with_body(&body));
        w.s3.multipart_begin(&mut w.io, token, "k", false).unwrap();
        assert_eq!(w.last(), bad_response(token), "{body:?}");
    }
    w.server
        .script(raw("HTTP/1.1 204 No Content\r\n\r\n".to_string()));
    w.s3.multipart_begin(&mut w.io, 5, "k", false).unwrap();
    assert_eq!(w.last(), bad_response(5));

    // An id of exactly the longest length is kept whole.
    let id = "i".repeat(longshore_s3::multipart::UPLOAD_ID_MAX);
    w.server.script(with_body(&format!(
        "<InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
    )));
    w.s3.multipart_begin(&mut w.io, 6, "k", false).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Begun {
            upload: UploadId::new(id.as_bytes()).unwrap()
        }
    );
}

/// A completion's answer is read for an error document even when its status
/// is 200. One too large to keep beside the head cannot be read, so it is
/// not taken for a success.
#[test]
fn a_completion_whose_answer_cannot_be_read_is_a_bad_response() {
    let mut w = world(12, Faults::default(), 3);
    let upload = w.begin(1, "k", false);
    let (len, _) = w.three_parts("k", &upload);
    let body = " ".repeat(HEAD_MAX as usize);
    w.server.script(raw(format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )));
    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, None)
        .unwrap();
    assert_eq!(w.last(), bad_response(20));
}
