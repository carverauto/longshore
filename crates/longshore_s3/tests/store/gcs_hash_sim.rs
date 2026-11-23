// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The forms `x-goog-hash` arrives in. Google Cloud Storage sends the
//! CRC-32C and the MD5 of an object as two values, which may share one
//! header line or have a line each, in either order.

#[expect(
    dead_code,
    unused_imports,
    reason = "`gcs_world` also serves the token-rotation and failure tests in gcs_sim.rs; these hash tests use only its world and server, so the rest goes unused here"
)]
#[cfg(test)]
mod gcs_world;
use gcs_world::*;

#[global_allocator]
static ALLOC: Guard = Guard;

/// What the store holds at the key.
const STORED: &[u8] = b"the object as the store holds it";
/// What the scripted response carries instead, so the test can tell which
/// response the caller was given.
const SCRIPTED: &[u8] = b"the body of the scripted response";
/// A well-formed MD5 value. Nothing reads it.
const MD5: &str = "md5=AAAAAAAAAAAAAAAAAAAAAA==";

#[cfg(test)]
fn crc_of(body: &[u8]) -> String {
    let value = longshore_crc::crc32c_header_value(longshore_crc::crc32c(body));
    format!("crc32c={}", core::str::from_utf8(&value).unwrap())
}

/// Fetch a key whose first response is `SCRIPTED` under these header lines.
/// Returns how the fetch ended and whether the caller got the scripted body.
#[cfg(test)]
fn fetch_with(hash_lines: &[String]) -> (Done, bool) {
    let mut w = world(1, Faults::default(), 3);
    w.io.buf(TX)[..STORED.len()].copy_from_slice(STORED);
    w.s3.store(
        &mut w.io,
        1,
        "k",
        TX,
        STORED.len() as u32,
        Checksum::Compute,
    )
    .unwrap();
    assert_eq!(w.last().outcome(), Outcome::Stored);
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n", SCRIPTED.len());
    for line in hash_lines {
        response += &format!("x-goog-hash: {line}\r\n");
    }
    response += "\r\n";
    let mut bytes = response.into_bytes();
    bytes.extend_from_slice(SCRIPTED);
    w.server
        .script(Behavior::Raw(Box::leak(bytes.into_boxed_slice())));
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    let done = w.last();
    let scripted = w.io.buf(RX)[..SCRIPTED.len()] == *SCRIPTED;
    (done, scripted)
}

fn verified_at_once(len: usize) -> Done {
    Done::new(
        2,
        Outcome::Fetched {
            len: len as u32,
            verified: true,
        },
        1,
    )
}

#[test]
fn both_values_on_one_line_are_read_in_either_order() {
    let crc = crc_of(SCRIPTED);
    for line in [
        format!("{crc},{MD5}"),
        format!("{MD5},{crc}"),
        format!("{MD5}, {crc}"),
        crc.clone(),
    ] {
        assert_eq!(
            fetch_with(std::slice::from_ref(&line)),
            (verified_at_once(SCRIPTED.len()), true),
            "{line}"
        );
    }
}

#[test]
fn a_value_on_a_line_of_its_own_is_read_when_it_comes_first() {
    let lines = [crc_of(SCRIPTED), MD5.to_string()];
    assert_eq!(fetch_with(&lines), (verified_at_once(SCRIPTED.len()), true));
}

/// A checksum that is not there, or is not the body's, is not accepted: the
/// object is fetched again, and the caller gets the store's bytes.
#[test]
fn a_missing_malformed_or_wrong_checksum_is_fetched_again() {
    let short = crc_of(SCRIPTED).trim_end_matches('=').to_string();
    for lines in [
        vec![],
        vec![MD5.to_string()],
        vec![short],
        vec![crc_of(STORED)],
    ] {
        let (done, scripted) = fetch_with(&lines);
        assert_eq!(
            done.outcome(),
            Outcome::Fetched {
                len: STORED.len() as u32,
                verified: true
            },
            "{lines:?}"
        );
        assert!(done.attempts() > 1 && !scripted, "{lines:?}: {done:?}");
    }
}

/// With a line each and the MD5 first, the CRC-32C is on the second
/// `x-goog-hash` line. Every line of that name is read, not only the first:
/// a response read as carrying no checksum would be fetched again where
/// checksums are required, and accepted unverified where they are not.
#[test]
fn a_value_on_a_line_of_its_own_is_read_when_it_comes_second() {
    let lines = [MD5.to_string(), crc_of(SCRIPTED)];
    assert_eq!(fetch_with(&lines), (verified_at_once(SCRIPTED.len()), true));
}
