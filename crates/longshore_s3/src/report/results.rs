// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters for what `put`, `head` and `get` report: [`Report`], [`Answer`], [`ContentRange`] and [`Signed`].

use super::{
    Answer, ContentRange, ETag, End, Report, RequestId, ServerEncryption, Signed, VersionId,
    SIGNED_HEADERS_MAX,
};
use crate::multipart::Fixed;

impl Report {
    /// How the operation ended.
    #[inline]
    pub const fn end(self) -> End {
        self.end
    }

    /// The last request handed to the `Io` for sending. `None` exactly
    /// when none was, which is when `sent` is zero.
    #[inline]
    pub const fn request(self) -> Option<Signed> {
        self.request
    }

    /// Requests handed to the `Io` for sending.
    #[inline]
    pub const fn sent(self) -> u32 {
        self.sent
    }

    /// Of those, the ones that ended with no answer from the store. Each
    /// may or may not have been carried out.
    #[inline]
    pub const fn unanswered(self) -> u32 {
        self.unanswered
    }
}
impl Answer {
    /// The response status code.
    #[inline]
    pub const fn status(self) -> u16 {
        self.status
    }

    /// `ETag`.
    #[inline]
    pub const fn etag(self) -> Option<ETag> {
        self.etag
    }

    /// `x-amz-version-id`, as sent. A bucket whose versioning is suspended
    /// answers `null`, and that is kept as those four characters.
    #[inline]
    pub const fn version(self) -> Option<VersionId> {
        self.version
    }

    /// `x-amz-request-id`.
    #[inline]
    pub const fn request_id(self) -> Option<RequestId> {
        self.request_id
    }

    /// `Content-Length`. For a `head` this is the object's length.
    #[inline]
    pub const fn content_length(self) -> Option<u64> {
        self.content_length
    }

    /// `Content-Range`.
    #[inline]
    pub const fn content_range(self) -> Option<ContentRange> {
        self.content_range
    }

    /// `x-amz-server-side-encryption`.
    #[inline]
    pub const fn encryption(self) -> Option<ServerEncryption> {
        self.encryption
    }

    /// `x-amz-checksum-sha256`, decoded.
    #[inline]
    pub const fn sha256(self) -> Option<[u8; 32]> {
        self.sha256
    }

    /// `x-amz-checksum-crc64nvme`, as its twelve base64 characters.
    #[inline]
    pub const fn crc64(self) -> Option<[u8; 12]> {
        self.crc64
    }

    /// Bytes of response body written into the caller's buffer. They are
    /// the object's bytes only when `status` is 200 or 206. Always zero for
    /// `put` and `head`.
    #[inline]
    pub const fn body_len(self) -> u32 {
        self.body_len
    }

    /// For a `get` of a whole object answered with 200 and a CRC-64/NVME:
    /// whether the bytes received match it. `None` when there was nothing
    /// to compare.
    #[inline]
    pub const fn verified(self) -> Option<bool> {
        self.verified
    }

    /// One of the headers above was present but did not parse or did not
    /// fit its type. Its field is `None`. Nothing is cut short.
    #[inline]
    pub const fn malformed(self) -> bool {
        self.malformed
    }
}
impl ContentRange {
    /// The first byte of the range.
    #[inline]
    pub const fn first(self) -> u64 {
        self.first
    }

    /// The last byte of the range, included.
    #[inline]
    pub const fn last(self) -> u64 {
        self.last
    }

    /// The whole object's length. `None` if the store wrote `*`.
    #[inline]
    pub const fn size(self) -> Option<u64> {
        self.size
    }
}
impl Signed {
    /// `PUT`, `HEAD` or `GET`.
    #[inline]
    pub const fn method(self) -> &'static str {
        self.method
    }

    /// The `x-amz-date` it carried: `YYYYMMDDTHHMMSSZ`.
    #[inline]
    pub const fn date(self) -> [u8; 16] {
        self.date
    }

    /// SHA-256 of the canonical request the signature was made over: the
    /// method, the path, the query, every signed header with its value,
    /// the list of their names, and the payload line. Rebuild it from the
    /// values expected with [`crate::sigv4::canonical_request_sha256`].
    #[inline]
    pub const fn canonical_request_sha256(self) -> [u8; 32] {
        self.canonical_request_sha256
    }

    /// The names of the signed headers, in lower case, joined by `;`.
    #[inline]
    pub const fn signed_headers(self) -> Fixed<SIGNED_HEADERS_MAX> {
        self.signed_headers
    }
}
