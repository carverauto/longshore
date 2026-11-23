// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Presigned URLs: a URL that lets its holder fetch or store one object for
//! a limited time without credentials of their own.
//!
//! Signing is computation only. Nothing is sent and nothing allocates; the
//! URL is written into a buffer the caller provides.

use crate::creds::Credentials;
use crate::sigv4::{self, uri_encode, Scope, UNSIGNED_PAYLOAD};
use crate::time::amz_date;

/// The longest life a store accepts for a presigned URL: seven days.
pub const EXPIRES_MAX_SECS: u32 = 7 * 24 * 60 * 60;

/// What the URL permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A GET of the object.
    Fetch,
    /// A PUT that creates or replaces the object.
    Store,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresignError {
    /// The key is empty.
    BadKey,
    /// The life asked for is zero or longer than [`EXPIRES_MAX_SECS`].
    BadExpiry,
    /// The credentials are a bearer token, which cannot sign a URL.
    NotSigningCredentials,
    /// The URL does not fit the buffer.
    OutputTooSmall,
}

/// Where the object is and who signs for it.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// `https` or `http`.
    pub scheme: &'a str,
    /// The host, as in the `Host` header: `bucket.endpoint`, or the endpoint
    /// alone when the bucket is in `root`, with `:port` if the port is not
    /// the scheme's default.
    pub host: &'a str,
    /// What the path starts with: empty when the bucket is in the host
    /// name, `/bucket` when it is addressed in the path.
    pub root: &'a str,
    pub region: &'a str,
    pub credentials: &'a Credentials,
}

/// Write a presigned URL for `key` into `out` and return its length. The URL
/// is valid for `expires_secs` from `unix_secs`.
pub fn presign(
    out: &mut [u8],
    target: &Target,
    access: Access,
    key: &str,
    unix_secs: u64,
    expires_secs: u32,
) -> Result<usize, PresignError> {
    check(target, key, expires_secs)?;
    let creds = target.credentials;
    let date = amz_date(unix_secs);
    let date_str = crate::text::written(&date);

    // Pieces that are signed and also appear in the URL, each encoded once.
    let mut path = [0u8; 64 + 1 + 3 * 1024];
    let path = encode_path(&mut path, target.root, key)?;
    let mut credential = [0u8; 256];
    let credential = write_credential(&mut credential, creds.access_key(), &date, target.region)?;
    let mut expires = [0u8; 10];
    let expires = write_decimal(&mut expires, expires_secs);
    let mut token = [0u8; 3 * crate::creds::SESSION_MAX];
    // Three times the longest token: always room, so `None` cannot happen.
    let token_len = uri_encode(creds.session_token().as_bytes(), true, &mut token)
        .ok_or(PresignError::OutputTooSmall)?;
    let token = crate::text::written(&token[..token_len]);

    let mut query = [
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256"),
        ("X-Amz-Credential", credential),
        ("X-Amz-Date", date_str),
        ("X-Amz-Expires", expires),
        ("X-Amz-Security-Token", token),
        ("X-Amz-SignedHeaders", "host"),
    ];
    // Without a session token the pair is left out. The rest is already in
    // the sorted order the signature needs.
    let pairs: &[(&str, &str)] = if token.is_empty() {
        query[4] = query[5];
        &query[..5]
    } else {
        &query
    };
    let method = match access {
        Access::Fetch => "GET",
        Access::Store => "PUT",
    };
    let signature = sigv4::signature(
        &sigv4::Request {
            method,
            canonical_uri: path,
            query: pairs,
            headers: &[("host", target.host)],
            payload_hash: UNSIGNED_PAYLOAD,
        },
        &Scope {
            access_key: creds.access_key(),
            secret_key: creds.secret_key(),
            region: target.region,
            service: "s3",
            amz_date: &date,
        },
    )
    .map_err(|_| PresignError::OutputTooSmall)?;
    write_url(out, target, path, pairs, &signature)
}

/// Refuse an empty key, a life out of range, or credentials that cannot sign.
fn check(target: &Target, key: &str, expires_secs: u32) -> Result<(), PresignError> {
    if key.is_empty() {
        return Err(PresignError::BadKey);
    }
    if expires_secs == 0 || expires_secs > EXPIRES_MAX_SECS {
        return Err(PresignError::BadExpiry);
    }
    if target.credentials.access_key().is_empty() {
        return Err(PresignError::NotSigningCredentials);
    }
    Ok(())
}

/// `root/key` with the key URI-encoded, written into `buf`.
fn encode_path<'b>(buf: &'b mut [u8], root: &str, key: &str) -> Result<&'b str, PresignError> {
    let at = root.len();
    buf.get_mut(..at)
        .ok_or(PresignError::BadKey)?
        .copy_from_slice(root.as_bytes());
    buf[at] = b'/';
    let len = at
        + 1
        + uri_encode(key.as_bytes(), false, &mut buf[at + 1..]).ok_or(PresignError::BadKey)?;
    Ok(crate::text::written(&buf[..len]))
}

/// The encoded `X-Amz-Credential` value, written into `buf`.
fn write_credential<'b>(
    buf: &'b mut [u8],
    access_key: &str,
    date: &[u8; 16],
    region: &str,
) -> Result<&'b str, PresignError> {
    let mut len = 0;
    for piece in [
        access_key.as_bytes(),
        b"%2F",
        &date[..8],
        b"%2F",
        region.as_bytes(),
        b"%2Fs3%2Faws4_request",
    ] {
        buf.get_mut(len..len + piece.len())
            .ok_or(PresignError::OutputTooSmall)?
            .copy_from_slice(piece);
        len += piece.len();
    }
    Ok(crate::text::written(&buf[..len]))
}

/// `v` in decimal, written at the end of `buf`.
fn write_decimal(buf: &mut [u8; 10], mut v: u32) -> &str {
    let mut d = buf.len();
    while v > 0 {
        d -= 1;
        buf[d] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    crate::text::written(&buf[d..])
}

/// Write the URL: scheme, host, path, query and signature.
fn write_url(
    out: &mut [u8],
    target: &Target,
    path: &str,
    pairs: &[(&str, &str)],
    signature: &[u8],
) -> Result<usize, PresignError> {
    let mut n = 0;
    let mut put = |bytes: &[u8]| -> Result<(), PresignError> {
        out.get_mut(n..n + bytes.len())
            .ok_or(PresignError::OutputTooSmall)?
            .copy_from_slice(bytes);
        n += bytes.len();
        Ok(())
    };
    put(target.scheme.as_bytes())?;
    put(b"://")?;
    put(target.host.as_bytes())?;
    put(path.as_bytes())?;
    for (k, (name, value)) in pairs.iter().enumerate() {
        put(if k == 0 { b"?" } else { b"&" })?;
        put(name.as_bytes())?;
        put(b"=")?;
        put(value.as_bytes())?;
    }
    put(b"&X-Amz-Signature=")?;
    put(signature)?;
    Ok(n)
}

#[cfg(test)]
mod tests;
