// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Presigned URLs: a URL that lets its holder fetch or store one object for
//! a limited time without credentials of their own.
//!
//! Signing is computation only. Nothing is sent and nothing allocates; the
//! URL is written into a buffer the caller provides.

use crate::store::creds::Credentials;
use crate::store::sigv4::{self, uri_encode, Scope, UNSIGNED_PAYLOAD};
use crate::store::time::amz_date;

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
    /// The bucket's host, as in the `Host` header: `bucket.endpoint`, with
    /// `:port` if the port is not the scheme's default.
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
    if key.is_empty() {
        return Err(PresignError::BadKey);
    }
    if expires_secs == 0 || expires_secs > EXPIRES_MAX_SECS {
        return Err(PresignError::BadExpiry);
    }
    let creds = target.credentials;
    if creds.access_key().is_empty() {
        return Err(PresignError::NotSigningCredentials);
    }
    let date = amz_date(unix_secs);
    let date_str = core::str::from_utf8(&date).expect("timestamps are ASCII");

    // Pieces that are signed and also appear in the URL, each encoded once.
    let mut path = [0u8; 64 + 1 + 3 * 1024];
    let at = target.root.len();
    path.get_mut(..at)
        .ok_or(PresignError::BadKey)?
        .copy_from_slice(target.root.as_bytes());
    path[at] = b'/';
    let path_len = at
        + 1
        + uri_encode(key.as_bytes(), false, &mut path[at + 1..]).ok_or(PresignError::BadKey)?;
    let path = core::str::from_utf8(&path[..path_len]).expect("encoded keys are ASCII");

    let mut credential = [0u8; 256];
    let mut n = 0;
    for piece in [
        creds.access_key().as_bytes(),
        b"%2F",
        &date[..8],
        b"%2F",
        target.region.as_bytes(),
        b"%2Fs3%2Faws4_request",
    ] {
        credential
            .get_mut(n..n + piece.len())
            .ok_or(PresignError::OutputTooSmall)?
            .copy_from_slice(piece);
        n += piece.len();
    }
    let credential = core::str::from_utf8(&credential[..n]).expect("ASCII");

    let mut expires = [0u8; 10];
    let mut d = expires.len();
    let mut v = expires_secs;
    while v > 0 {
        d -= 1;
        expires[d] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let expires = core::str::from_utf8(&expires[d..]).expect("digits");

    let mut token = [0u8; 3 * crate::store::creds::SESSION_MAX];
    let token_len = uri_encode(creds.session_token().as_bytes(), true, &mut token)
        .expect("an encoded session token fits three times its length");
    let token = core::str::from_utf8(&token[..token_len]).expect("ASCII");

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
    put(&signature)?;
    Ok(n)
}

#[cfg(test)]
mod tests;
