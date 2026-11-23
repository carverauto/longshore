// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Azure Storage Shared Key authorization for the Blob service.
//!
//! The signature covers the request head only: the method, a fixed list of
//! standard header values, the `x-ms-` headers and the resource. The body is
//! never hashed. Nothing here allocates.
//!
//! Follows "Authorize with Shared Key" in the Azure Storage REST reference
//! (service version 2015-02-21 and later). Checked against the examples
//! there and accepted by the Azurite emulator for every Blob request the
//! client makes (`longshore-bench/examples/azure_probe.rs`); not yet run against a
//! real storage account.

use crate::crypto::hmac_sha256;
use longshore_crc::base64;

/// `x-ms-` headers one request may carry.
pub const HEADERS_MAX: usize = 8;
pub const QUERY_MAX: usize = 8;
/// Longest string to sign.
pub const STRING_MAX: usize = 2048;
/// An account key is 64 bytes, 88 characters of base64.
pub const KEY_MAX: usize = 64;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SignError {
    TooManyHeaders,
    TooManyQueryParameters,
    /// The string to sign or the Authorization value does not fit.
    TooLong,
}

pub struct Request<'a> {
    pub(crate) method: &'a str,
    pub(crate) content_length: u64,
    pub(crate) content_type: &'a str,
    pub(crate) if_none_match: &'a str,
    pub(crate) range: &'a str,
    pub(crate) ms_headers: &'a [(&'a str, &'a str)],
    pub(crate) account: &'a str,
    pub(crate) path: &'a str,
    pub(crate) query: &'a [(&'a str, &'a str)],
}

struct Out<'a> {
    buf: &'a mut [u8],
    len: usize,
    overflow: bool,
}

impl Out<'_> {
    fn put(&mut self, bytes: &[u8]) {
        match self.buf.get_mut(self.len..self.len + bytes.len()) {
            Some(dst) => {
                dst.copy_from_slice(bytes);
                self.len += bytes.len();
            }
            None => self.overflow = true,
        }
    }

    fn line(&mut self, value: &str) {
        self.put(value.as_bytes());
        self.put(b"\n");
    }
}

/// Writes the string that is signed into `out`; returns its length.
pub fn string_to_sign(req: &Request, out: &mut [u8]) -> Result<usize, SignError> {
    if req.ms_headers.len() > HEADERS_MAX {
        return Err(SignError::TooManyHeaders);
    }
    if req.query.len() > QUERY_MAX {
        return Err(SignError::TooManyQueryParameters);
    }
    let mut headers = [("", ""); HEADERS_MAX];
    headers[..req.ms_headers.len()].copy_from_slice(req.ms_headers);
    let headers = &mut headers[..req.ms_headers.len()];
    headers.sort_unstable_by_key(|h| h.0);
    let mut query = [("", ""); QUERY_MAX];
    query[..req.query.len()].copy_from_slice(req.query);
    let query = &mut query[..req.query.len()];
    query.sort_unstable_by_key(|q| q.0);

    let mut length = [0u8; 20];
    let mut at = length.len();
    let mut v = req.content_length;
    while v > 0 {
        at -= 1;
        length[at] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let length = crate::text::written(&length[at..]);

    let mut o = Out {
        buf: out,
        len: 0,
        overflow: false,
    };
    o.line(req.method);
    o.line(""); // Content-Encoding
    o.line(""); // Content-Language
    o.line(length); // Content-Length, empty when zero
    o.line(""); // Content-MD5
    o.line(req.content_type);
    o.line(""); // Date: x-ms-date is used instead
    o.line(""); // If-Modified-Since
    o.line(""); // If-Match
    o.line(req.if_none_match);
    o.line(""); // If-Unmodified-Since
    o.line(req.range);
    for (name, value) in headers.iter() {
        o.put(name.as_bytes());
        o.put(b":");
        o.line(value);
    }
    o.put(b"/");
    o.put(req.account.as_bytes());
    o.put(req.path.as_bytes());
    for (name, value) in query.iter() {
        o.put(b"\n");
        o.put(name.as_bytes());
        o.put(b":");
        o.put(value.as_bytes());
    }
    if o.overflow {
        return Err(SignError::TooLong);
    }
    Ok(o.len)
}

/// Writes the Authorization header value, `SharedKey account:signature`,
/// into `out`; returns its length. `key` is the decoded account key.
pub fn authorization(req: &Request, key: &[u8], out: &mut [u8]) -> Result<usize, SignError> {
    let mut string = [0u8; STRING_MAX];
    let n = string_to_sign(req, &mut string)?;
    let mac = hmac_sha256(key, &string[..n]);
    let mut signature = [0u8; 44];
    let _: usize = base64(&mac, &mut signature);
    let mut o = Out {
        buf: out,
        len: 0,
        overflow: false,
    };
    o.put(b"SharedKey ");
    o.put(req.account.as_bytes());
    o.put(b":");
    o.put(&signature);
    if o.overflow {
        return Err(SignError::TooLong);
    }
    Ok(o.len)
}

/// Decodes an account key from the base64 form the portal shows.
/// Returns the key's length, or `None` if it is not valid padded base64 or
/// is longer than [`KEY_MAX`].
pub fn decode_key(text: &str, out: &mut [u8; KEY_MAX]) -> Option<usize> {
    let text = text.as_bytes();
    if text.is_empty() || !text.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    };
    let mut n = 0;
    for (i, quad) in text.chunks(4).enumerate() {
        let last = i == text.len() / 4 - 1;
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut bits = 0u32;
        for &c in &quad[..4 - pad] {
            bits = bits << 6 | value(c)?;
        }
        bits <<= 6 * pad as u32;
        // Bits that padding says are unused must be zero.
        if pad > 0 && bits & (if pad == 1 { 0xff } else { 0xffff }) != 0 {
            return None;
        }
        let bytes = bits.to_be_bytes();
        for &b in &bytes[1..4 - pad] {
            *out.get_mut(n)? = b;
            n += 1;
        }
    }
    Some(n)
}

#[cfg(test)]
mod tests;

impl<'a> Request<'a> {
    /// A `method` request to `path` in `account`, with no body, no
    /// `x-ms-` headers, no query and none of the optional headers; the
    /// `with_*` setters add them.
    #[inline]
    pub const fn new(method: &'a str, account: &'a str, path: &'a str) -> Self {
        Request {
            method,
            content_length: 0,
            content_type: "",
            if_none_match: "",
            range: "",
            ms_headers: &[],
            account,
            path,
            query: &[],
        }
    }

    /// Uppercase.
    #[inline]
    pub const fn method(&self) -> &'a str {
        self.method
    }

    /// This value with [`Request::method`] set to `method`.
    #[inline]
    pub const fn with_method(mut self, method: &'a str) -> Self {
        self.method = method;
        self
    }

    /// Signed as an empty line when zero.
    #[inline]
    pub const fn content_length(&self) -> u64 {
        self.content_length
    }

    /// This value with [`Request::content_length`] set to `content_length`.
    #[inline]
    pub const fn with_content_length(mut self, content_length: u64) -> Self {
        self.content_length = content_length;
        self
    }

    /// The `Content-Type` header, or empty.
    #[inline]
    pub const fn content_type(&self) -> &'a str {
        self.content_type
    }

    /// This value with [`Request::content_type`] set to `content_type`.
    #[inline]
    pub const fn with_content_type(mut self, content_type: &'a str) -> Self {
        self.content_type = content_type;
        self
    }

    /// The `If-None-Match` header, or empty.
    #[inline]
    pub const fn if_none_match(&self) -> &'a str {
        self.if_none_match
    }

    /// This value with [`Request::if_none_match`] set to `if_none_match`.
    #[inline]
    pub const fn with_if_none_match(mut self, if_none_match: &'a str) -> Self {
        self.if_none_match = if_none_match;
        self
    }

    /// The `Range` header, or empty.
    #[inline]
    pub const fn range(&self) -> &'a str {
        self.range
    }

    /// This value with [`Request::range`] set to `range`.
    #[inline]
    pub const fn with_range(mut self, range: &'a str) -> Self {
        self.range = range;
        self
    }

    /// Every `x-ms-` header the request carries, including `x-ms-date`:
    /// lowercase names, any order, each name once.
    #[inline]
    pub const fn ms_headers(&self) -> &'a [(&'a str, &'a str)] {
        self.ms_headers
    }

    /// This value with [`Request::ms_headers`] set to `ms_headers`.
    #[inline]
    pub const fn with_ms_headers(mut self, ms_headers: &'a [(&'a str, &'a str)]) -> Self {
        self.ms_headers = ms_headers;
        self
    }

    /// The storage account name.
    #[inline]
    pub const fn account(&self) -> &'a str {
        self.account
    }

    /// This value with [`Request::account`] set to `account`.
    #[inline]
    pub const fn with_account(mut self, account: &'a str) -> Self {
        self.account = account;
        self
    }

    /// The path exactly as sent, starting with `/`, without the query.
    #[inline]
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// This value with [`Request::path`] set to `path`.
    #[inline]
    pub const fn with_path(mut self, path: &'a str) -> Self {
        self.path = path;
        self
    }

    /// Query parameters: lowercase names, decoded values, any order, each name once.
    #[inline]
    pub const fn query(&self) -> &'a [(&'a str, &'a str)] {
        self.query
    }

    /// This value with [`Request::query`] set to `query`.
    #[inline]
    pub const fn with_query(mut self, query: &'a [(&'a str, &'a str)]) -> Self {
        self.query = query;
        self
    }
}
