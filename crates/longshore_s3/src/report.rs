// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What `put`, `head` and `get` ask of a store and what they report back.
//!
//! These three operations speak in the store's own terms. They do not turn
//! a status into success or failure. A [`Report`] says whether a request
//! was sent, what the store answered if it did, and which request that was,
//! and the caller decides what it means. Every field of an [`Answer`] comes
//! from the response: one the store did not send is `None`, never filled in
//! from what was asked for.
//!
//! All of it is `Copy` and holds its bytes inline, so a report crosses from
//! an I/O thread to its caller without allocating.

use crate::multipart::Fixed;
use crate::s3::Checksum;
use longshore_http_client::client::Error;

mod options;
mod results;

/// Longest `ETag` kept, quotes included.
pub const ETAG_MAX: usize = 258;
/// Longest version id kept. S3 allows them up to this length.
pub const VERSION_ID_MAX: usize = 1024;
/// Longest request id kept.
pub const REQUEST_ID_MAX: usize = 128;
/// Room for the names of the headers one request signs.
pub const SIGNED_HEADERS_MAX: usize = 256;

/// An object's `ETag` as the store sent it, quotes included.
pub type ETag = Fixed<ETAG_MAX>;
/// The id of one version of an object.
pub type VersionId = Fixed<VERSION_ID_MAX>;
/// The store's own name for one request, for its operators to look up.
pub type RequestId = Fixed<REQUEST_ID_MAX>;

/// Encryption the store is asked to apply to an object at rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encryption {
    /// With keys the store manages: `x-amz-server-side-encryption: AES256`.
    Aes256,
}

/// What the store said about an object's encryption at rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerEncryption {
    /// `AES256`.
    Aes256,
    /// Some other value, such as one that names a key service.
    Other,
}

/// How to store an object with `put`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Put {
    pub(crate) no_clobber: bool,
    pub(crate) encryption: Option<Encryption>,
    pub(crate) checksum: Checksum,
}

impl Default for Put {
    /// Replace any object at the key, ask for no encryption, and send a
    /// CRC-64/NVME computed over the body.
    fn default() -> Put {
        Put {
            no_clobber: false,
            encryption: None,
            checksum: Checksum::Compute,
        }
    }
}

/// The object a `head` or `get` must find. The store refuses the request
/// if what is at the key is something else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Expect {
    pub(crate) etag: Option<ETag>,
    pub(crate) version: Option<VersionId>,
}

/// How far one operation may go, in the store's own clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub(crate) attempts: u32,
    pub(crate) deadline_ns: Option<u64>,
}

/// How a `put`, `head` or `get` ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub(crate) end: End,
    pub(crate) request: Option<Signed>,
    pub(crate) sent: u32,
    pub(crate) unanswered: u32,
}

#[expect(
    clippy::large_enum_variant,
    reason = "a report is held in a slot that exists whether or not it is used, so boxing the large variant would only add an allocation"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// Nothing was sent. The store cannot have acted.
    NotSent(Cause),
    /// The store answered the last request sent, with whatever status.
    /// With more than one attempt allowed, a later attempt may have ended
    /// before it was sent: the deadline passed, or no connection opened.
    Answered(Answer),
    /// The last request sent got no answer. The store may or may not have
    /// acted on it.
    Unknown(Cause),
}

/// Why an operation ended without an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The operation's deadline passed.
    Deadline,
    /// The store's name never resolved to an address.
    NoAddress,
    /// The connection or the exchange on it failed.
    Transport(Error),
    /// The I/O under the store failed.
    Closed,
}

/// What the store's response said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Answer {
    pub(crate) status: u16,
    pub(crate) etag: Option<ETag>,
    pub(crate) version: Option<VersionId>,
    pub(crate) request_id: Option<RequestId>,
    pub(crate) content_length: Option<u64>,
    pub(crate) content_range: Option<ContentRange>,
    pub(crate) encryption: Option<ServerEncryption>,
    pub(crate) sha256: Option<[u8; 32]>,
    pub(crate) crc64: Option<[u8; 12]>,
    pub(crate) body_len: u32,
    pub(crate) verified: Option<bool>,
    pub(crate) malformed: bool,
}

/// A `Content-Range: bytes first-last/size` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentRange {
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) size: Option<u64>,
}

impl ContentRange {
    /// Read the header's value. `None` unless it is exactly
    /// `bytes first-last/size` with `first <= last < size`, or with `*`
    /// for the size.
    pub fn parse(value: &[u8]) -> Option<ContentRange> {
        let text = core::str::from_utf8(value).ok()?.strip_prefix("bytes ")?;
        let (range, size) = text.split_once('/')?;
        let (first, last) = range.split_once('-')?;
        let number = |digits: &str| -> Option<u64> {
            // `str::parse` would also take a leading plus sign.
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| digits.parse().ok())
                .flatten()
        };
        let (first, last) = (number(first)?, number(last)?);
        let size = match size {
            "*" => None,
            digits => Some(number(digits)?),
        };
        (first <= last && size.is_none_or(|size| last < size)).then_some(ContentRange {
            first,
            last,
            size,
        })
    }
}

/// The request that was signed and handed to the `Io` for sending. It
/// holds nothing secret: no signature, no key and no session token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signed {
    pub(crate) method: &'static str,
    pub(crate) date: [u8; 16],
    pub(crate) canonical_request_sha256: [u8; 32],
    pub(crate) signed_headers: Fixed<SIGNED_HEADERS_MAX>,
}

/// Decode base64 with padding into exactly `N` bytes. `None` for any
/// other length, a character outside the alphabet, or bits left over.
pub(crate) fn base64_decode<const N: usize>(text: &[u8]) -> Option<[u8; N]> {
    if text.len() != N.div_ceil(3) * 4 {
        return None;
    }
    let padding = (3 - N % 3) % 3;
    let (data, pad) = text.split_at(text.len() - padding);
    if pad.iter().any(|b| *b != b'=') {
        return None;
    }
    let mut out = [0u8; N];
    let (mut bits, mut count, mut n) = (0u32, 0u32, 0usize);
    for &b in data {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(v);
        count += 6;
        if count >= 8 {
            count -= 8;
            out[n] = (bits >> count) as u8;
            n += 1;
        }
    }
    // What is left must be the zero bits that padding stands for.
    (n == N && bits & ((1 << count) - 1) == 0).then_some(out)
}

#[cfg(test)]
mod tests;
