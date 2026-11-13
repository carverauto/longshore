// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! SigV4 against cases from the official AWS signing test suite
//! (awslabs/aws-c-auth, tests/aws-signing-test-suite/v4), copied into
//! `tests/sigv4/`. Each case's request is parsed from `request.txt` and the
//! signature must equal `header-signature.txt`.

use longshore::store::sigv4::{authorization, signature, uri_encode, Request, Scope, EMPTY_SHA256};
use longshore::store::time::amz_date;

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

fn case(name: &str) -> (String, String, bool) {
    let dir = format!("{}/tests/store/sigv4/{name}", env!("CARGO_MANIFEST_DIR"));
    let read = |f: &str| {
        std::fs::read_to_string(format!("{dir}/{f}")).unwrap_or_else(|e| panic!("{dir}/{f}: {e}"))
    };
    // `sign_body` cases also sign an x-amz-content-sha256 header.
    let sign_body = read("context.json")
        .replace(' ', "")
        .contains("\"sign_body\":true");
    (
        read("request.txt"),
        read("header-signature.txt").trim().to_string(),
        sign_body,
    )
}

/// Sign the request in `request.txt` the way a caller would.
fn sign(request: &str, sign_body: bool) -> (String, String) {
    let mut lines = request.lines();
    let start = lines.next().unwrap();
    let (method, rest) = start.split_once(' ').unwrap();
    let target = rest.rsplit_once(' ').unwrap().0;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers: Vec<(String, String)> = lines
        .take_while(|l| !l.is_empty())
        .map(|l| {
            l.split_once(':')
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .unwrap()
        })
        .collect();
    let body = request
        .split_once("\n\n")
        .map_or("", |x| x.1)
        .trim_end_matches('\n');

    // The suite's timestamp, produced by our own formatter.
    let date = amz_date(1_440_938_160);
    let date_str = String::from_utf8(date.to_vec()).unwrap();
    let payload = if body.is_empty() {
        EMPTY_SHA256.to_string()
    } else {
        longshore::crypto::sha256(body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let mut signed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    signed.push(("X-Amz-Date", &date_str));

    let enc = |s: &str, slash: bool| {
        let mut out = vec![0u8; s.len() * 3 + 1];
        let n = uri_encode(s.as_bytes(), slash, &mut out).unwrap();
        String::from_utf8(out[..n].to_vec()).unwrap()
    };
    let canonical_uri = enc(path, false);
    let pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .map(|(n, v)| (enc(n, true), enc(v, true)))
        .collect();
    let query: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();

    if sign_body {
        signed.push(("X-Amz-Content-Sha256", &payload));
    }
    let req = Request {
        method,
        canonical_uri: &canonical_uri,
        query: &query,
        headers: &signed,
        payload_hash: &payload,
    };
    let scope = Scope {
        access_key: "AKIDEXAMPLE",
        secret_key: SECRET,
        region: "us-east-1",
        service: "service",
        amz_date: &date,
    };
    let sig = String::from_utf8(signature(&req, &scope).unwrap().to_vec()).unwrap();
    let mut auth = [0u8; 512];
    let n = authorization(&req, &scope, &mut auth).unwrap();
    (sig, String::from_utf8(auth[..n].to_vec()).unwrap())
}

#[test]
fn matches_the_official_test_suite() {
    let mut failures = Vec::new();
    for name in [
        "get-vanilla",
        "get-vanilla-query-order-key-case",
        "get-unreserved",
        "get-space-normalized",
        "post-x-www-form-urlencoded-parameters",
        "get-header-value-trim",
        "get-header-value-order",
        "get-header-key-duplicate",
        "get-utf8",
        "get-vanilla-utf8-query",
        "get-vanilla-empty-query-key",
    ] {
        let (request, expected, sign_body) = case(name);
        let (sig, auth) = sign(&request, sign_body);
        if sig != expected || !auth.ends_with(&format!("Signature={expected}")) {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "wrong signature for: {failures:?}");
}

#[test]
fn authorization_header_has_the_documented_shape() {
    let (request, expected, sign_body) = case("get-vanilla");
    let (_, auth) = sign(&request, sign_body);
    assert_eq!(auth, format!("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature={expected}"));
}
