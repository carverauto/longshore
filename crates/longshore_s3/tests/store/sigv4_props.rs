// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) for SigV4: the percent-encoding, and what a
//! signature does and does not depend on.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_s3::sigv4::{authorization, signature, uri_encode, Request, Scope, UNSIGNED_PAYLOAD};

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~".contains(&b)
}

/// Percent-decoding, written here from RFC 3986. `None` unless every `%`
/// is followed by two uppercase hex digits.
fn decode(encoded: &[u8]) -> Option<Vec<u8>> {
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let mut out = Vec::new();
    let mut rest = encoded;
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'%' {
            out.push(digit(*tail.first()?)? << 4 | digit(*tail.get(1)?)?);
            rest = &tail[2..];
        } else {
            out.push(b);
            rest = tail;
        }
    }
    Some(out)
}

/// Any bytes encode to unreserved characters, escapes, and `/` where a
/// path keeps it; decoding gives the bytes back; and the output needs
/// exactly the room it takes.
#[hegel::test]
fn encoding_is_reversible_and_uses_only_what_a_signature_allows(tc: TestCase) {
    let input = tc.draw(gs::binary().max_size(200));
    let encode_slash = tc.draw(gs::booleans());
    let kept = |b: u8| unreserved(b) || (b == b'/' && !encode_slash);
    let need: usize = input.iter().map(|b| if kept(*b) { 1 } else { 3 }).sum();

    let mut out = vec![0u8; need + 8];
    assert_eq!(uri_encode(&input, encode_slash, &mut out), Some(need));
    let encoded = &out[..need];
    assert!(
        encoded.iter().all(|b| kept(*b) || *b == b'%'),
        "{:?}",
        String::from_utf8_lossy(encoded)
    );
    assert_eq!(decode(encoded).as_deref(), Some(&input[..]));

    // Room for exactly that, and no less.
    let mut exact = vec![0u8; need];
    assert_eq!(uri_encode(&input, encode_slash, &mut exact), Some(need));
    assert_eq!(exact, encoded);
    if need > 0 {
        let short = tc.draw(gs::integers::<usize>().min_value(0).max_value(need - 1));
        assert_eq!(
            uri_encode(&input, encode_slash, &mut exact[..short]),
            None,
            "{short} of {need} bytes"
        );
    }
    // Encoding what is already encoded escapes the percent signs again.
    let mut twice = vec![0u8; need * 3];
    let n = uri_encode(encoded, encode_slash, &mut twice).unwrap();
    assert_eq!(decode(&twice[..n]).as_deref(), Some(encoded));
}

const SCOPE: Scope<'static> = Scope {
    access_key: "AKIDEXAMPLE",
    secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
    region: "us-east-1",
    service: "s3",
    amz_date: b"20150830T123600Z",
};

/// A word of 1 to 8 characters from `alphabet`.
fn word(tc: &TestCase, alphabet: &[u8]) -> String {
    let picks = tc.draw(gs::binary().min_size(1).max_size(8));
    picks
        .iter()
        .map(|b| alphabet[*b as usize % alphabet.len()] as char)
        .collect()
}

/// `items` in an order the test case chooses.
fn shuffled<T: Clone>(tc: &TestCase, items: &[T]) -> Vec<T> {
    let mut v = items.to_vec();
    for i in (1..v.len()).rev() {
        let j = tc.draw(gs::integers::<usize>().min_value(0).max_value(i));
        v.swap(i, j);
    }
    v
}

fn borrowed(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect()
}

fn sign(method: &str, headers: &[(String, String)], query: &[(String, String)]) -> String {
    let (h, q) = (borrowed(headers), borrowed(query));
    let req = Request {
        method,
        canonical_uri: "/dir/key",
        query: &q,
        headers: &h,
        payload_hash: UNSIGNED_PAYLOAD,
    };
    let sig = signature(&req, &SCOPE).unwrap();
    // The header value ends with the same signature.
    let mut auth = [0u8; 2048];
    let n = authorization(&req, &SCOPE, &mut auth).unwrap();
    assert!(auth[..n].ends_with(&sig));
    String::from_utf8(sig.to_vec()).unwrap()
}

/// A signature depends on what the headers and the query say, not on how
/// the caller happens to give them: their order, the case of header names,
/// and the whitespace around and between the words of a value.
#[hegel::test]
fn a_signature_depends_on_content_not_on_presentation(tc: TestCase) {
    const NAME: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";
    const VALUE: &[u8] = b"abcXYZ019/+=;:,.\"'()*_~!@#$%^&";
    // Distinct names: each starts with its own index.
    let headers: Vec<(String, String)> = (0..tc
        .draw(gs::integers::<usize>().min_value(1).max_value(8)))
        .map(|i| {
            let words = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
            let value: Vec<String> = (0..words).map(|_| word(&tc, VALUE)).collect();
            (format!("x-{i}-{}", word(&tc, NAME)), value.join(" "))
        })
        .collect();
    let query: Vec<(String, String)> = (0..tc
        .draw(gs::integers::<usize>().min_value(0).max_value(5)))
        .map(|i| (format!("q{i}{}", word(&tc, NAME)), word(&tc, NAME)))
        .collect();
    let base = sign("PUT", &headers, &query);

    let spaces = |min: usize| -> String {
        let n = tc.draw(gs::integers::<usize>().min_value(min).max_value(3));
        (0..n)
            .map(|_| if tc.draw(gs::booleans()) { ' ' } else { '\t' })
            .collect()
    };
    let presented: Vec<(String, String)> = shuffled(&tc, &headers)
        .into_iter()
        .map(|(name, value)| {
            let name = if tc.draw(gs::booleans()) {
                name.to_ascii_uppercase()
            } else {
                name
            };
            let mut padded = spaces(0);
            for (k, w) in value.split(' ').enumerate() {
                if k > 0 {
                    padded += &spaces(1);
                }
                padded += w;
            }
            padded += &spaces(0);
            (name, padded)
        })
        .collect();
    assert_eq!(sign("PUT", &presented, &shuffled(&tc, &query)), base);

    // What is said does matter: one changed character, anywhere.
    assert_ne!(sign("GET", &headers, &query), base);
    let at = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(headers.len() - 1),
    );
    let mut changed = headers.clone();
    changed[at].1.push('x');
    assert_ne!(sign("PUT", &changed, &query), base);
    let mut renamed = headers.clone();
    renamed[at].0.push('x');
    assert_ne!(sign("PUT", &renamed, &query), base);
    let mut dropped = headers.clone();
    let _: (String, String) = dropped.remove(at);
    if !dropped.is_empty() {
        assert_ne!(sign("PUT", &dropped, &query), base);
    }
    if !query.is_empty() {
        let mut changed_query = query.clone();
        changed_query[0].1.push('x');
        assert_ne!(sign("PUT", &headers, &changed_query), base);
    }
}

/// Values of a repeated name are joined in the order given, so that order
/// is part of what is signed.
#[hegel::test]
fn the_order_of_a_repeated_headers_values_is_signed(tc: TestCase) {
    const VALUE: &[u8] = b"abcdefgh";
    let (a, b) = (word(&tc, VALUE), word(&tc, VALUE));
    let pair = |x: &str, y: &str| {
        vec![
            ("host".to_string(), "h".to_string()),
            ("x-dup".to_string(), x.to_string()),
            ("X-Dup".to_string(), y.to_string()),
        ]
    };
    let joined = vec![
        ("host".to_string(), "h".to_string()),
        ("x-dup".to_string(), format!("{a},{b}")),
    ];
    assert_eq!(sign("GET", &pair(&a, &b), &[]), sign("GET", &joined, &[]));
    assert_eq!(
        sign("GET", &pair(&a, &b), &[]) == sign("GET", &pair(&b, &a), &[]),
        a == b
    );
}
