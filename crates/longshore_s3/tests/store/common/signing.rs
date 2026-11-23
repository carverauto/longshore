// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A check of SigV4 signatures for the test server, which cannot make one
//! itself because it does not know the secret.
//!
//! It rebuilds the signed request from the bytes that arrived, not from
//! what the store meant to send. So a header sent but left out of the
//! signature, a query signed in another form than the one in the target, or
//! a path signed differently from the path requested, is refused here as a
//! real store would refuse it.

use longshore_http_client::head::Fields;
use longshore_s3::sigv4::{self, Request, Scope, HEADERS_MAX, QUERY_MAX};

/// The secret of each access key the tests sign with. A request under any
/// other key is not judged.
pub const SECRETS: &[(&str, &str)] = &[("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY")];

/// Headers a store requires to be signed whenever they are sent.
fn must_be_signed(name: &str) -> bool {
    let name = name.as_bytes();
    name.len() >= 6 && name[..6].eq_ignore_ascii_case(b"x-amz-")
        || [
            &b"host"[..],
            b"range",
            b"if-match",
            b"if-none-match",
            b"content-md5",
        ]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// Whether the request carries a SigV4 signature that matches it. Does not
/// allocate: the server calls it while allocation is sealed.
pub fn verify_sigv4(method: &[u8], target: &[u8], head: &[u8], fields: &Fields) -> bool {
    check(method, target, head, fields).unwrap_or(false)
}

fn check(method: &[u8], target: &[u8], head: &[u8], fields: &Fields) -> Option<bool> {
    let text = |b| core::str::from_utf8(b).ok();
    let header = |name: &str| fields.header(head, name).and_then(text);
    let auth = header("authorization")?.strip_prefix("AWS4-HMAC-SHA256 ")?;
    let mut parts = auth.split(", ");
    let credential = parts.next()?.strip_prefix("Credential=")?;
    let signed = parts.next()?.strip_prefix("SignedHeaders=")?;
    let signature = parts.next()?.strip_prefix("Signature=")?;
    if parts.next().is_some() {
        return Some(false);
    }
    let mut scope = credential.split('/');
    let (access_key, date, region, service) =
        (scope.next()?, scope.next()?, scope.next()?, scope.next()?);
    if scope.next() != Some("aws4_request") || scope.next().is_some() {
        return Some(false);
    }
    let Some((_, secret_key)) = SECRETS.iter().find(|(key, _)| *key == access_key) else {
        return Some(true);
    };
    let amz_date: &[u8; 16] = header("x-amz-date")?.as_bytes().try_into().ok()?;
    if &amz_date[..8] != date.as_bytes() {
        return Some(false);
    }

    // The signed headers, with the values that arrived.
    let mut headers = [("", ""); HEADERS_MAX];
    let mut count = 0;
    for name in signed.split(';') {
        if count == HEADERS_MAX {
            return Some(false);
        }
        headers[count] = (name, header(name)?);
        count += 1;
    }
    // Every header that must be signed is among them.
    let lines = text(head)?.split("\r\n").skip(1);
    for name in lines.filter_map(|line| line.split_once(':').map(|(name, _)| name)) {
        if must_be_signed(name) && !signed.split(';').any(|s| s.eq_ignore_ascii_case(name)) {
            return Some(false);
        }
    }

    // The path and query exactly as requested.
    let target = text(target)?;
    let (path, query_text) = target.split_once('?').unwrap_or((target, ""));
    let mut query = [("", ""); QUERY_MAX];
    let mut pairs = 0;
    for pair in query_text.split('&').filter(|p| !p.is_empty()) {
        if pairs == QUERY_MAX {
            return Some(false);
        }
        query[pairs] = pair.split_once('=').unwrap_or((pair, ""));
        pairs += 1;
    }

    let expected = sigv4::signature(
        &Request::new(
            text(method)?,
            path,
            &query[..pairs],
            &headers[..count],
            header("x-amz-content-sha256")?,
        ),
        &Scope::new(access_key, secret_key, region, service, amz_date),
    )
    .ok()?;
    Some(signature.as_bytes() == expected)
}
