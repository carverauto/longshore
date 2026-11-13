// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart upload against the in-tree HTTP server on one simulated `Io`,
//! with heap allocation forbidden while operations run.

mod common;
use common::*;
use longshore::store::multipart::{write_manifest, Part, UploadId};

const PART: u32 = 4000;

/// The bytes part `number` is filled with.
fn part_bytes(number: u32) -> Vec<u8> {
    (0..PART - number)
        .map(|i| i.wrapping_mul(2654435761).to_le_bytes()[1] ^ number as u8)
        .collect()
}

impl World {
    fn begin(&mut self, token: u64, key: &str, checksums: bool) -> UploadId {
        self.s3
            .multipart_begin(&mut self.io, token, key, checksums)
            .unwrap();
        match self.last().outcome {
            Outcome::Begun { upload } => upload,
            other => panic!("expected an upload, got {other:?}"),
        }
    }

    /// Submit part `number` from `buf` without waiting for it.
    fn send_part(&mut self, token: u64, key: &str, upload: &UploadId, number: u32, buf: BufId) {
        let bytes = part_bytes(number);
        self.io.buf(buf)[..bytes.len()].copy_from_slice(&bytes);
        self.s3
            .multipart_store_part(
                &mut self.io,
                token,
                key,
                upload,
                number,
                buf,
                bytes.len() as u32,
                Checksum::Compute,
            )
            .unwrap();
    }

    /// Part `number` as completion lists it, from the outcome of `token`.
    fn part_of(&self, number: u32, token: u64) -> Part {
        match self
            .done
            .iter()
            .find(|d| d.token == token)
            .map(|d| d.outcome)
        {
            Some(Outcome::PartStored { tag, checksum }) => {
                // Every part here is sent with a computed CRC-64/NVME.
                let crc = longshore::checksum::crc64_nvme(&part_bytes(number));
                assert_eq!(checksum, Some(crc));
                Part {
                    number,
                    tag,
                    checksum: checksum.map(longshore::checksum::crc64_header_value),
                }
            }
            other => panic!("expected a stored part for token {token}, got {other:?}"),
        }
    }

    /// Store parts 1 to 3 and write their manifest into `TX`. Returns its
    /// length and the whole object.
    fn three_parts(&mut self, key: &str, upload: &UploadId) -> (u32, Vec<u8>) {
        // Part 2 first, then 1 and 3 at the same time from two buffers.
        self.send_part(12, key, upload, 2, TX);
        self.last();
        self.send_part(11, key, upload, 1, TX);
        self.send_part(13, key, upload, 3, RX);
        self.run_until_done(self.done.len() + 2);
        let parts = [
            self.part_of(1, 11),
            self.part_of(2, 12),
            self.part_of(3, 13),
        ];
        let len = write_manifest(self.io.buf(TX), &parts).unwrap() as u32;
        let object = [part_bytes(1), part_bytes(2), part_bytes(3)].concat();
        (len, object)
    }
}

#[test]
fn an_upload_in_parts_becomes_one_verified_object() {
    let mut w = world_with(1, Faults::default(), 3, true);
    let upload = w.begin(1, "big", true);
    let (len, object) = w.three_parts("big", &upload);
    let crc = longshore::checksum::crc64_nvme(&object);
    w.s3.multipart_complete(&mut w.io, 20, "big", &upload, TX, len, Some(crc))
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 20,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
    w.s3.fetch(&mut w.io, 21, "big", None, RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: object.len() as u32,
            verified: true
        }
    );
    assert!(w.io.buf(RX)[..object.len()] == object[..]);
    // The parts are gone and only the object is listed.
    w.s3.list(&mut w.io, 22, "", None, 100, RX).unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("listing failed")
    };
    let listing = Listing::parse(&w.io.buf(RX)[..len as usize]).unwrap();
    let keys: Vec<&[u8]> = listing.entries().map(|e| e.unwrap().key_raw).collect();
    assert_eq!(keys, [b"big"]);
}

#[test]
fn an_aborted_upload_leaves_nothing_and_aborting_twice_is_fine() {
    let mut w = world(2, Faults::default(), 3);
    let upload = w.begin(1, "k", false);
    w.send_part(2, "k", &upload, 1, TX);
    assert!(matches!(w.last().outcome, Outcome::PartStored { .. }));
    w.s3.multipart_abort(&mut w.io, 3, "k", &upload).unwrap();
    assert_eq!(w.last().outcome, Outcome::Aborted);
    // The store no longer knows the upload: still aborted.
    w.s3.multipart_abort(&mut w.io, 4, "k", &upload).unwrap();
    assert_eq!(w.last().outcome, Outcome::Aborted);
    w.s3.fetch(&mut w.io, 5, "k", None, RX).unwrap();
    assert_eq!(w.last().outcome, Outcome::NotFound);
    // A part for an upload that is gone is refused, not stored.
    w.send_part(6, "k", &upload, 1, TX);
    assert_eq!(w.last().outcome, Outcome::Fatal(Fatal::Status(404)));
    assert_eq!(
        w.s3.multipart_store_part(&mut w.io, 7, "k", &upload, 0, TX, 1, Checksum::None),
        Err(SubmitError::BadPart)
    );
    assert_eq!(
        w.s3.multipart_store_part(&mut w.io, 7, "k", &upload, 10_001, TX, 1, Checksum::None),
        Err(SubmitError::BadPart)
    );
}

#[test]
fn every_phase_is_retried_through_a_failure() {
    let mut w = world_with(3, Faults::default(), 4, true);
    w.server.script(Behavior::CloseBeforeResponse);

    w.s3.multipart_begin(&mut w.io, 1, "k", true).unwrap();
    let d = w.last();
    let Outcome::Begun { upload } = d.outcome else {
        panic!("expected an upload, got {d:?}")
    };
    assert_eq!(d.attempts, 2);

    w.server.script(Behavior::Status(503));
    w.server.script(Behavior::CloseBeforeResponse);

    w.send_part(12, "k", &upload, 2, TX);
    let d = w.last();
    assert!(matches!(d.outcome, Outcome::PartStored { .. }));
    assert_eq!(d.attempts, 3);
    w.send_part(11, "k", &upload, 1, TX);
    w.send_part(13, "k", &upload, 3, RX);
    w.run_until_done(w.done.len() + 2);
    let parts = [w.part_of(1, 11), w.part_of(2, 12), w.part_of(3, 13)];
    let len = write_manifest(w.io.buf(TX), &parts).unwrap() as u32;

    // Status 200 with an error document is a failure. A transient one is retried.
    w.server
        .script(Behavior::ErrorDocument(200, "InternalError"));

    let object = [part_bytes(1), part_bytes(2), part_bytes(3)].concat();
    let crc = longshore::checksum::crc64_nvme(&object);
    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, Some(crc))
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 20,
            outcome: Outcome::Stored,
            attempts: 2
        }
    );
    w.s3.fetch(&mut w.io, 21, "k", None, RX).unwrap();
    assert!(matches!(w.last().outcome, Outcome::Fetched { .. }));
    assert!(w.io.buf(RX)[..object.len()] == object[..]);
}

#[test]
fn an_error_document_that_will_not_improve_ends_the_completion() {
    let mut w = world(4, Faults::default(), 3);
    let upload = w.begin(1, "k", false);
    let (len, _) = w.three_parts("k", &upload);
    w.server.script(Behavior::ErrorDocument(200, "InvalidPart"));

    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, None)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 20,
            outcome: Outcome::Fatal(Fatal::ErrorDocument),
            attempts: 1
        }
    );
}

#[test]
fn a_completion_whose_response_was_lost_is_confirmed_by_the_object_checksum() {
    let mut w = world_with(5, Faults::default(), 4, true);
    let upload = w.begin(1, "k", true);
    let (len, object) = w.three_parts("k", &upload);
    let crc = longshore::checksum::crc64_nvme(&object);
    // The store completes the upload and the response never arrives. The
    // retry is told the upload does not exist.
    w.server.script(Behavior::ApplyThenClose);

    w.s3.multipart_complete(&mut w.io, 20, "k", &upload, TX, len, Some(crc))
        .unwrap();
    let d = w.last();
    assert_eq!(d.outcome, Outcome::Stored);
    assert_eq!(w.s3.store_comparisons, 1);

    // Without a checksum there is nothing to confirm it with.
    let upload = w.begin(2, "other", false);
    let (len, _) = w.three_parts("other", &upload);
    w.server.script(Behavior::ApplyThenClose);

    w.s3.multipart_complete(&mut w.io, 30, "other", &upload, TX, len, None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Fatal(Fatal::Status(404)));

    // A wrong whole-object checksum is refused by the store.
    let upload = w.begin(3, "third", true);
    let (len, _) = w.three_parts("third", &upload);
    w.s3.multipart_complete(&mut w.io, 40, "third", &upload, TX, len, Some(crc ^ 1))
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Fatal(Fatal::Status(400)));
}

#[test]
fn a_damaged_part_is_refused_and_an_overlong_upload_id_is_an_error() {
    let mut w = world_with(6, Faults::default(), 2, true);
    let upload = w.begin(1, "k", true);
    let bytes = part_bytes(1);
    w.io.buf(TX)[..bytes.len()].copy_from_slice(&bytes);
    let wrong = longshore::checksum::crc64_nvme(&bytes) ^ 1;
    w.s3.multipart_store_part(
        &mut w.io,
        2,
        "k",
        &upload,
        1,
        TX,
        bytes.len() as u32,
        Checksum::Provided(wrong),
    )
    .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: 2
        }
    );

    // An upload id longer than the type holds is reported, never cut short.
    let id = "a".repeat(300);
    let body = format!(
        "<InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    w.server.script(Behavior::Raw(Box::leak(
        response.into_bytes().into_boxed_slice(),
    )));

    w.s3.multipart_begin(&mut w.io, 3, "k2", false).unwrap();
    assert_eq!(w.last().outcome, Outcome::Fatal(Fatal::BadResponse));
}

#[test]
fn a_part_sent_without_a_checksum_is_listed_without_one() {
    let mut w = world_with(5, Faults::default(), 3, true);
    let upload = w.begin(1, "plain", false);
    let bytes = part_bytes(1);
    w.io.buf(TX)[..bytes.len()].copy_from_slice(&bytes);
    w.s3.multipart_store_part(
        &mut w.io,
        2,
        "plain",
        &upload,
        1,
        TX,
        bytes.len() as u32,
        Checksum::None,
    )
    .unwrap();
    let Outcome::PartStored { tag, checksum } = w.last().outcome else {
        panic!("expected a stored part, got {:?}", w.last())
    };
    assert_eq!(checksum, None);
    let part = Part {
        number: 1,
        tag,
        checksum: None,
    };
    let len = write_manifest(w.io.buf(TX), &[part]).unwrap() as u32;
    w.s3.multipart_complete(&mut w.io, 3, "plain", &upload, TX, len, None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
}
