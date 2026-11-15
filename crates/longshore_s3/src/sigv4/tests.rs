// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn uri_encoding_rules() {
    let enc = |s: &str, slash: bool| {
        let mut out = [0u8; 128];
        let n = uri_encode(s.as_bytes(), slash, &mut out).unwrap();
        String::from_utf8(out[..n].to_vec()).unwrap()
    };
    assert_eq!(
        enc("/photos/Jan sample.jpg", false),
        "/photos/Jan%20sample.jpg"
    );
    assert_eq!(enc("a/b", true), "a%2Fb");
    assert_eq!(enc("AZaz09-._~", true), "AZaz09-._~");
    assert_eq!(enc("+=&*'()", true), "%2B%3D%26%2A%27%28%29");
    assert_eq!(
        enc("é", false),
        "%C3%A9",
        "UTF-8 bytes are encoded individually, uppercase hex"
    );
    assert_eq!(uri_encode(b"a b", false, &mut [0u8; 4]), None);
}

const DATE: &[u8; 16] = b"20150830T123600Z";

fn scope() -> Scope<'static> {
    Scope {
        access_key: "AKIDEXAMPLE",
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        region: "us-east-1",
        service: "s3",
        amz_date: DATE,
    }
}

fn sign(headers: &[(&str, &str)], query: &[(&str, &str)]) -> Result<[u8; 64], SignError> {
    signature(
        &Request {
            method: "GET",
            canonical_uri: "/k",
            query,
            headers,
            payload_hash: UNSIGNED_PAYLOAD,
        },
        &scope(),
    )
}

/// A server strips the spaces and tabs around a header value before it
/// sees it, and the canonical form joins the words of a value with single
/// spaces. A tab is whitespace like a space is: signing it as it stands
/// gives a signature the store cannot reproduce.
#[test]
fn tabs_in_a_header_value_are_whitespace_like_spaces() {
    let with = |value: &'static str| {
        sign(
            &[("host", "example.amazonaws.com"), ("x-amz-meta-k", value)],
            &[],
        )
        .unwrap()
    };
    assert_eq!(with("a\t"), with("a"), "trailing tab");
    assert_eq!(with("\ta"), with("a"), "leading tab");
    assert_eq!(with(" \t a \t "), with("a"), "tabs among spaces");
    assert_eq!(with("a \t b"), with("a b"), "a tab between words");
    assert_eq!(with("a\tb"), with("a b"), "a tab alone between words");
    assert_ne!(with("ab"), with("a b"));
}

/// The signature of a canonical request written out by hand, by the steps
/// of the SigV4 specification and with none of the signer's code: only the
/// hash functions are shared.
fn signature_of(canonical_request: &str) -> [u8; 64] {
    use crate::crypto::{hex, hmac_sha256, sha256};
    let s = scope();
    let date = core::str::from_utf8(&DATE[..8]).unwrap();
    let mut hashed = [0u8; 64];
    hex(&sha256(canonical_request.as_bytes()), &mut hashed);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{date}/{}/{}/aws4_request\n{}",
        core::str::from_utf8(DATE).unwrap(),
        s.region,
        s.service,
        core::str::from_utf8(&hashed).unwrap(),
    );
    let key = hmac_sha256(format!("AWS4{}", s.secret_key).as_bytes(), date.as_bytes());
    let key = hmac_sha256(&key, s.region.as_bytes());
    let key = hmac_sha256(&key, s.service.as_bytes());
    let key = hmac_sha256(&key, b"aws4_request");
    let mut out = [0u8; 64];
    hex(&hmac_sha256(&key, to_sign.as_bytes()), &mut out);
    out
}

/// What a value with tabs is signed as, against a canonical request that
/// is written here and not produced by the signer. The tests above compare
/// the signer with itself, so they would pass a canonical form that was
/// wrong in the same way for both sides. The form is the one AWS's own
/// SDKs produce: the words of the value joined by single spaces, where a
/// tab separates words as a space does.
#[test]
fn a_value_with_tabs_is_signed_as_its_words_joined_by_single_spaces() {
    let expected = signature_of(
        "GET\n/k\n\nhost:example.amazonaws.com\nx-amz-meta-k:a b c\n\n\
         host;x-amz-meta-k\nUNSIGNED-PAYLOAD",
    );
    for value in ["a b c", "a\tb\tc", "\ta \t b\t\tc \t", "  a\t \tb   c\t"] {
        let signed = sign(
            &[("host", "example.amazonaws.com"), ("x-amz-meta-k", value)],
            &[],
        )
        .unwrap();
        assert_eq!(signed, expected, "{value:?}");
    }
    // And the hand-written route agrees with the signer where no tab is
    // involved, on a vector of the official suite's shape.
    assert_eq!(
        sign(
            &[("host", "example.amazonaws.com")],
            &[("b", "2"), ("a", "1")]
        )
        .unwrap(),
        signature_of("GET\n/k\na=1&b=2\nhost:example.amazonaws.com\n\nhost\nUNSIGNED-PAYLOAD"),
    );
    // A canonical form that kept the tab, or dropped the separator, is a
    // different request.
    for wrong in ["a\tb\tc", "abc", "a  b  c"] {
        assert_ne!(
            expected,
            signature_of(&format!(
                "GET\n/k\n\nhost:example.amazonaws.com\nx-amz-meta-k:{wrong}\n\n\
                 host;x-amz-meta-k\nUNSIGNED-PAYLOAD"
            ))
        );
    }
}

#[test]
fn refuses_more_headers_or_query_pairs_than_it_can_sort() {
    let headers = [("x-h", "v"); HEADERS_MAX + 1];
    let query = [("q", "v"); QUERY_MAX + 1];
    assert!(sign(&headers[..HEADERS_MAX], &[]).is_ok());
    assert_eq!(sign(&headers, &[]), Err(SignError::TooManyHeaders));
    assert!(sign(&[("host", "h")], &query[..QUERY_MAX]).is_ok());
    assert_eq!(
        sign(&[("host", "h")], &query),
        Err(SignError::TooManyQueryParameters)
    );
}

#[test]
fn the_authorization_value_fits_exactly_or_is_refused() {
    let req = Request {
        method: "GET",
        canonical_uri: "/k",
        query: &[],
        headers: &[("Host", "h"), ("X-Amz-Date", "d"), ("host", "again")],
        payload_hash: UNSIGNED_PAYLOAD,
    };
    let mut out = [0u8; 512];
    let n = authorization(&req, &scope(), &mut out).unwrap();
    let text = core::str::from_utf8(&out[..n]).unwrap();
    // A name given twice is listed once.
    assert!(
        text.contains(", SignedHeaders=host;x-amz-date, Signature="),
        "{text}"
    );
    assert_eq!(authorization(&req, &scope(), &mut out[..n]), Ok(n));
    for short in [0, 1, n / 2, n - 64, n - 1] {
        assert_eq!(
            authorization(&req, &scope(), &mut out[..short]),
            Err(SignError::OutputTooSmall),
            "{short} of {n} bytes"
        );
    }
}

#[test]
fn uri_encoding_at_the_edges() {
    let enc = |input: &[u8], slash: bool| {
        let mut out = [0u8; 64];
        let n = uri_encode(input, slash, &mut out).unwrap();
        String::from_utf8(out[..n].to_vec()).unwrap()
    };
    assert_eq!(enc(b"", true), "");
    assert_eq!(
        enc(b"%", true),
        "%25",
        "a percent sign is not passed through"
    );
    assert_eq!(enc(b"%41", false), "%2541");
    assert_eq!(enc(&[0x00, 0x7f, 0x80, 0xff], true), "%00%7F%80%FF");
    assert_eq!(enc(b"/", false), "/");
    assert_eq!(enc(b"/", true), "%2F");
    assert_eq!(enc(b" ", true), "%20", "a space is never a plus sign");
    // The output fits exactly or not at all, whichever kind of byte is last.
    assert_eq!(uri_encode(b"ab", false, &mut [0u8; 2]), Some(2));
    assert_eq!(uri_encode(b"ab", false, &mut [0u8; 1]), None);
    assert_eq!(uri_encode(b"a ", false, &mut [0u8; 4]), Some(4));
    assert_eq!(uri_encode(b"a ", false, &mut [0u8; 3]), None);
    assert_eq!(uri_encode(b"", false, &mut []), Some(0));
}
