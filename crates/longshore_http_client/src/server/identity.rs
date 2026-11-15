// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What identifies a stored object and what the server says about it: its
//! version, its encryption and its SHA-256, the preconditions a request
//! puts on them, and the response headers that carry them.

use super::*;

/// A SHA-256 checksum header's value: forty-four base64 characters.
pub(super) const SHA256_LEN: usize = 44;
/// Room for an `If-Match` value: an entity tag of up to 256 characters
/// and its quotes.
const IF_MATCH_MAX: usize = 258;

/// Maps a body to the value its `x-amz-checksum-sha256` header must have.
pub type Sha256Fn = fn(&[u8]) -> [u8; SHA256_LEN];

/// What a request asked about an object's identity.
#[derive(Clone, Copy)]
pub(super) struct Asked {
    if_match: ([u8; IF_MATCH_MAX], usize),
    /// The `versionId` the target carried; zero for none.
    version: u32,
    /// `x-amz-server-side-encryption: AES256` was sent.
    pub encrypt: bool,
    /// The `x-amz-checksum-sha256` sent with the request.
    pub sha256: Option<[u8; SHA256_LEN]>,
    /// The object whose identity goes on the response, and the range of it
    /// the response carries.
    pub reply: Option<u32>,
    pub reply_range: Option<(u32, u32)>,
}

impl Asked {
    pub(super) const NOTHING: Asked = Asked {
        if_match: ([0; IF_MATCH_MAX], 0),
        version: 0,
        encrypt: false,
        sha256: None,
        reply: None,
        reply_range: None,
    };

    /// Read the request's identity headers, and take a `versionId` query
    /// off `target`. Returns the target's length without it.
    pub(super) fn read<'a>(
        header: impl Fn(&str) -> Option<&'a [u8]>,
        target: &[u8],
    ) -> (Asked, usize) {
        let mut asked = Asked::NOTHING;
        if let Some(v) = header("if-match") {
            let n = v.len().min(IF_MATCH_MAX + 1);
            let kept = v.len().min(IF_MATCH_MAX);
            asked.if_match.0[..kept].copy_from_slice(&v[..kept]);
            asked.if_match.1 = n;
        }
        asked.encrypt = header("x-amz-server-side-encryption") == Some(b"AES256");
        asked.sha256 = header("x-amz-checksum-sha256").and_then(|v| v.try_into().ok());
        let name = b"?versionId=";
        let Some(at) = target.windows(name.len()).position(|w| w == name) else {
            return (asked, target.len());
        };
        // A version this server never gave out matches nothing.
        asked.version = core::str::from_utf8(&target[at + name.len()..])
            .ok()
            .and_then(|v| v.strip_prefix("sim-v")?.parse().ok())
            .unwrap_or(u32::MAX);
        (asked, at)
    }
}

/// What the server keeps about an object's identity.
#[derive(Clone, Copy, Default)]
pub(super) struct Kept {
    /// Zero when versions are not kept.
    pub version: u32,
    pub encrypted: bool,
    pub sha256: Option<[u8; SHA256_LEN]>,
}

impl Server {
    /// Give every stored object a version id, as a bucket with versioning
    /// on does, and serve an object only under the version it has now.
    pub fn keep_versions(&mut self) {
        self.versions = Some(0);
    }

    /// Store objects without encrypting them even when asked to, and so
    /// never say they are encrypted: a store that ignores the request.
    pub fn ignore_encryption(&mut self) {
        self.encrypts = false;
    }

    /// Refuse a store whose `x-amz-checksum-sha256` is not what `sum`
    /// gives for its body, with a `BadDigest` error document.
    pub fn check_sha256(&mut self, sum: Sha256Fn) {
        self.sha256 = Some(sum);
    }

    /// Whether a stored body agrees with the SHA-256 claimed for it.
    pub(super) fn sha256_agrees(&self, asked: &Asked, body: &[u8]) -> bool {
        match (self.sha256, asked.sha256) {
            (Some(sum), Some(claimed)) => sum(body) == claimed,
            _ => true,
        }
    }

    /// What to keep about an object stored by a request that asked `asked`.
    pub(super) fn keep(&mut self, asked: &Asked) -> Kept {
        if let Some(last) = &mut self.versions {
            *last += 1;
        }
        Kept {
            version: self.versions.unwrap_or(0),
            encrypted: asked.encrypt && self.encrypts,
            sha256: asked.sha256,
        }
    }

    /// The status that refuses a fetch of object `slot`, if `asked` names
    /// another version of it or another tag.
    pub(super) fn refused(&self, asked: &Asked, slot: usize) -> Option<(u16, &'static str)> {
        let o = &self.objects[slot];
        if asked.version != 0 && asked.version != o.kept.version {
            return Some((404, "Not Found"));
        }
        let (tag, n) = asked.if_match;
        (n > 0 && tag.get(..n) != Some(&multipart::tag(&o.data[..o.len as usize])[..]))
            .then_some((412, "Precondition Failed"))
    }

    /// Write the headers that identify the object the response is about.
    pub(super) fn identify(&mut self, w: &mut HeadWriter, i: usize) {
        let asked = &mut self.conns[i].asked;
        let (Some(slot), range) = (asked.reply.take(), asked.reply_range.take()) else {
            return;
        };
        let o = &self.objects[slot as usize];
        let mut digits = [0u8; 48];
        let mut n = 0;
        let mut put = |bytes: &[u8]| {
            digits[n..n + bytes.len()].copy_from_slice(bytes);
            n += bytes.len();
        };
        let decimal = |v: u32| {
            let mut out = [0u8; 10];
            let mut at = out.len();
            let mut v = v;
            loop {
                at -= 1;
                out[at] = b'0' + (v % 10) as u8;
                v /= 10;
                if v == 0 {
                    return (out, at);
                }
            }
        };
        if o.kept.version != 0 {
            let (d, at) = decimal(o.kept.version);
            put(b"sim-v");
            put(&d[at..]);
            w.header("x-amz-version-id", &digits[..n]);
        }
        if o.kept.encrypted {
            w.header("x-amz-server-side-encryption", b"AES256");
        }
        if let Some(sum) = o.kept.sha256 {
            w.header("x-amz-checksum-sha256", &sum);
        }
        if let Some((first, last)) = range {
            let mut value = [0u8; 48];
            let mut n = 0;
            for part in [
                (b"bytes ".as_slice(), None),
                (b"", Some(first)),
                (b"-", Some(last)),
                (b"/", Some(o.len)),
            ] {
                value[n..n + part.0.len()].copy_from_slice(part.0);
                n += part.0.len();
                if let Some(v) = part.1 {
                    let (d, at) = decimal(v);
                    value[n..n + d.len() - at].copy_from_slice(&d[at..]);
                    n += d.len() - at;
                }
            }
            w.header("Content-Range", &value[..n]);
        }
        let (d, at) = decimal(self.requests as u32);
        let mut id = *b"sim-request-0000000000";
        let start = id.len() - (d.len() - at);
        id[start..].copy_from_slice(&d[at..]);
        w.header("x-amz-request-id", &id);
    }
}
