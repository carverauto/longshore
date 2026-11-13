// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! AWS Signature Version 4, header-based (S3 and S3-compatible stores).
//!
//! The canonical request is hashed as it is produced, so signing needs no
//! buffer proportional to the request and never allocates. Only the request
//! head is signed; the body is declared `UNSIGNED-PAYLOAD` (or covered by a
//! checksum header) so that large bodies are never hashed with SHA-256.

use osc_crypto::{hex, hmac_sha256, Sha256};

pub const HEADERS_MAX: usize = 16;
pub const QUERY_MAX: usize = 16;
/// Value of `x-amz-content-sha256` when the body is not signed.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
/// Hex SHA-256 of the empty string.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SignError {
    TooManyHeaders,
    TooManyQueryParameters,
    /// The output buffer is too small for the Authorization value.
    OutputTooSmall,
}

pub struct Request<'a> {
    pub method: &'a str,
    /// Already URI-encoded path, e.g. from [`uri_encode`] with `encode_slash = false`.
    pub canonical_uri: &'a str,
    /// Already URI-encoded names and values, in any order.
    pub query: &'a [(&'a str, &'a str)],
    /// Headers to sign, in any order and any case. Must include `host`.
    pub headers: &'a [(&'a str, &'a str)],
    /// Value for the last line of the canonical request.
    pub payload_hash: &'a str,
}

pub struct Scope<'a> {
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    /// `YYYYMMDDTHHMMSSZ`.
    pub amz_date: &'a [u8; 16],
}

/// Indices of `items` ordered by `key`, without allocating.
fn sorted<const N: usize, T>(items: &[T], less: impl Fn(&T, &T) -> bool) -> [usize; N] {
    let mut idx = [0usize; N];
    for (i, slot) in idx.iter_mut().enumerate().take(items.len()) {
        *slot = i;
    }
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 && less(&items[idx[j]], &items[idx[j - 1]]) {
            idx.swap(j, j - 1);
            j -= 1;
        }
    }
    idx
}

fn lower_less(a: &str, b: &str) -> bool {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .lt(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Feed a header value with leading/trailing spaces removed and inner runs
/// of spaces collapsed to one.
fn update_trimmed(h: &mut Sha256, value: &str) {
    let mut pending_space = false;
    let mut started = false;
    for b in value.bytes() {
        if b == b' ' {
            pending_space = started;
            continue;
        }
        if pending_space {
            h.update(b" ");
            pending_space = false;
        }
        started = true;
        h.update(&[b]);
    }
}

fn update_lower(h: &mut Sha256, s: &str) {
    for b in s.bytes() {
        h.update(&[b.to_ascii_lowercase()]);
    }
}

/// Hex signature for a request.
pub fn signature(req: &Request, scope: &Scope) -> Result<[u8; 64], SignError> {
    if req.headers.len() > HEADERS_MAX {
        return Err(SignError::TooManyHeaders);
    }
    if req.query.len() > QUERY_MAX {
        return Err(SignError::TooManyQueryParameters);
    }
    let headers: [usize; HEADERS_MAX] = sorted(req.headers, |a, b| lower_less(a.0, b.0));
    let query: [usize; QUERY_MAX] = sorted(req.query, |a, b| (a.0, a.1) < (b.0, b.1));

    // Canonical request, hashed as it is generated.
    let mut h = Sha256::new();
    h.update(req.method.as_bytes())
        .update(b"\n")
        .update(req.canonical_uri.as_bytes())
        .update(b"\n");
    for (n, i) in query[..req.query.len()].iter().enumerate() {
        let (name, value) = req.query[*i];
        if n > 0 {
            h.update(b"&");
        }
        h.update(name.as_bytes())
            .update(b"=")
            .update(value.as_bytes());
    }
    h.update(b"\n");
    // A name given more than once is one header: its values are joined
    // with commas in the order given (the sort is stable), and the name is
    // listed once among the signed headers.
    let order = &headers[..req.headers.len()];
    let repeats = |k: usize| {
        k > 0
            && req.headers[order[k - 1]]
                .0
                .eq_ignore_ascii_case(req.headers[order[k]].0)
    };
    for (k, i) in order.iter().enumerate() {
        let (name, value) = req.headers[*i];
        if repeats(k) {
            h.update(b",");
        } else {
            if k > 0 {
                h.update(b"\n");
            }
            update_lower(&mut h, name);
            h.update(b":");
        }
        update_trimmed(&mut h, value);
    }
    if !order.is_empty() {
        h.update(b"\n");
    }
    h.update(b"\n");
    let mut first = true;
    for (k, i) in order.iter().enumerate() {
        if repeats(k) {
            continue;
        }
        if !first {
            h.update(b";");
        }
        first = false;
        update_lower(&mut h, req.headers[*i].0);
    }
    h.update(b"\n").update(req.payload_hash.as_bytes());
    let mut request_hash = [0u8; 64];
    hex(&h.finish(), &mut request_hash);

    // String to sign: algorithm, timestamp, credential scope, request hash.
    let date = &scope.amz_date[..8];
    let mut sts = [0u8; 256];
    let mut n = 0;
    let mut put = |b: &[u8]| -> Result<(), SignError> {
        sts.get_mut(n..n + b.len())
            .ok_or(SignError::OutputTooSmall)?
            .copy_from_slice(b);
        n += b.len();
        Ok(())
    };
    put(b"AWS4-HMAC-SHA256\n")?;
    put(scope.amz_date)?;
    put(b"\n")?;
    put(date)?;
    put(b"/")?;
    put(scope.region.as_bytes())?;
    put(b"/")?;
    put(scope.service.as_bytes())?;
    put(b"/aws4_request\n")?;
    put(&request_hash)?;

    let mut out = [0u8; 64];
    hex(&hmac_sha256(&signing_key(scope), &sts[..n]), &mut out);
    Ok(out)
}

/// The per-day, per-region, per-service key.
fn signing_key(scope: &Scope) -> [u8; 32] {
    let mut k0 = [0u8; 4 + 128];
    let secret = scope.secret_key.as_bytes();
    assert!(secret.len() <= 128, "secret key longer than 128 bytes");
    k0[..4].copy_from_slice(b"AWS4");
    k0[4..4 + secret.len()].copy_from_slice(secret);
    let k_date = hmac_sha256(&k0[..4 + secret.len()], &scope.amz_date[..8]);
    let k_region = hmac_sha256(&k_date, scope.region.as_bytes());
    let k_service = hmac_sha256(&k_region, scope.service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Write the `Authorization` header value into `out`. Returns its length.
pub fn authorization(req: &Request, scope: &Scope, out: &mut [u8]) -> Result<usize, SignError> {
    let sig = signature(req, scope)?;
    let headers: [usize; HEADERS_MAX] = sorted(req.headers, |a, b| lower_less(a.0, b.0));
    let mut n = 0;
    let mut put = |b: &[u8]| -> Result<(), SignError> {
        out.get_mut(n..n + b.len())
            .ok_or(SignError::OutputTooSmall)?
            .copy_from_slice(b);
        n += b.len();
        Ok(())
    };
    put(b"AWS4-HMAC-SHA256 Credential=")?;
    put(scope.access_key.as_bytes())?;
    put(b"/")?;
    put(&scope.amz_date[..8])?;
    put(b"/")?;
    put(scope.region.as_bytes())?;
    put(b"/")?;
    put(scope.service.as_bytes())?;
    put(b"/aws4_request, SignedHeaders=")?;
    let order = &headers[..req.headers.len()];
    for (k, i) in order.iter().enumerate() {
        // A repeated name is listed once, as it is in the signature.
        if k > 0
            && req.headers[order[k - 1]]
                .0
                .eq_ignore_ascii_case(req.headers[*i].0)
        {
            continue;
        }
        if k > 0 {
            put(b";")?;
        }
        for b in req.headers[*i].0.bytes() {
            put(&[b.to_ascii_lowercase()])?;
        }
    }
    put(b", Signature=")?;
    put(&sig)?;
    Ok(n)
}

/// Percent-encode per the SigV4 rules: unreserved characters
/// `A-Z a-z 0-9 - . _ ~` pass through, everything else becomes `%XX` with
/// uppercase hex. `/` is kept for object key paths and encoded for query values.
/// Returns the encoded length, or `None` if `out` is too small.
pub fn uri_encode(input: &[u8], encode_slash: bool, out: &mut [u8]) -> Option<usize> {
    let mut n = 0;
    for &b in input {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (b == b'/' && !encode_slash)
        {
            *out.get_mut(n)? = b;
            n += 1;
        } else {
            let dst = out.get_mut(n..n + 3)?;
            dst[0] = b'%';
            dst[1] = b"0123456789ABCDEF"[(b >> 4) as usize];
            dst[2] = b"0123456789ABCDEF"[(b & 15) as usize];
            n += 3;
        }
    }
    Some(n)
}

#[cfg(test)]
mod tests;
