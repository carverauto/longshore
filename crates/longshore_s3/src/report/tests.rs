// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The examples of RFC 7233 section 4.2, and the forms it rules out.
#[test]
fn content_range_reads_the_rfc_forms_and_nothing_else() {
    let range = |first, last, size| Some(ContentRange { first, last, size });
    assert_eq!(
        ContentRange::parse(b"bytes 42-1233/1234"),
        range(42, 1233, Some(1234))
    );
    assert_eq!(
        ContentRange::parse(b"bytes 42-1233/*"),
        range(42, 1233, None)
    );
    assert_eq!(ContentRange::parse(b"bytes 0-0/1"), range(0, 0, Some(1)));
    // An unsatisfied range, a last byte at or past the length, a first
    // byte after the last, another unit, signs and missing parts.
    for bad in [
        &b"bytes */1234"[..],
        b"bytes 0-1234/1234",
        b"bytes 5-4/10",
        b"items 0-1/2",
        b"bytes +0-1/2",
        b"bytes 0-1",
        b"bytes -1/2",
        b"bytes 0-/2",
        b"bytes 0-1/",
        b"0-1/2",
        b"",
    ] {
        assert_eq!(ContentRange::parse(bad), None, "{bad:?}");
    }
}

/// The test vectors of RFC 4648 section 10.
#[test]
fn base64_decodes_the_rfc_vectors_at_their_exact_length() {
    assert_eq!(base64_decode::<1>(b"Zg=="), Some(*b"f"));
    assert_eq!(base64_decode::<2>(b"Zm8="), Some(*b"fo"));
    assert_eq!(base64_decode::<3>(b"Zm9v"), Some(*b"foo"));
    assert_eq!(base64_decode::<6>(b"Zm9vYmFy"), Some(*b"foobar"));
    // The wrong length for the type, a character outside the alphabet,
    // padding in the wrong place and bits set under the padding.
    assert_eq!(base64_decode::<3>(b"Zm8="), None);
    assert_eq!(base64_decode::<2>(b"Zm9v"), None);
    assert_eq!(base64_decode::<2>(b"Zm-="), None);
    assert_eq!(base64_decode::<2>(b"Z=8="), None);
    assert_eq!(base64_decode::<2>(b"Zm9="), None);
    assert_eq!(base64_decode::<1>(b"Zh=="), None);
}

/// SHA-256 of the empty string, as FIPS 180-4's examples give it, in the
/// base64 a store would send.
#[test]
fn base64_decodes_a_sha256_header_value() {
    let digest: [u8; 32] = [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ];
    assert_eq!(
        base64_decode(b"47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="),
        Some(digest)
    );
}
