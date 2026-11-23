// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Reading what a parsed request head asks for into its connection.

use super::*;

/// Judges a request by its method, its target exactly as sent, the bytes of
/// its head and the parsed fields. Called while requests run, so it must
/// not allocate.
pub type VerifyFn = fn(method: &[u8], target: &[u8], head: &[u8], fields: &Fields) -> bool;

impl Server {
    /// Record on connection `i` what the request head in `buf` asks for.
    /// Returns whether its method or target is too long to keep.
    pub(super) fn note_request(&mut self, i: usize, buf: &[u8], head: &RequestHead) -> bool {
        let method = head.method(buf);
        let target = in_bucket(head.target(buf), self.bucket.as_bytes());
        let authorized = self.authorizes(buf, head);
        let c = &mut self.conns[i];
        let too_long = c.note_target(buf, head.fields(), method, target);
        c.note_fields(buf, head.fields());
        c.authorized = authorized;
        too_long
    }

    /// Whether the request is allowed: its access key is on the list, if
    /// there is one, the test's check accepts it, and it carries the
    /// session token, if one is required.
    fn authorizes(&self, buf: &[u8], head: &RequestHead) -> bool {
        let (allowed_keys, allowed_count) = (&self.allowed_keys, self.allowed_count);
        let mut authorized = allowed_count == 0
            || head.fields().header(buf, "authorization").is_some_and(|v| {
                // "... Credential=<access key>/<date>/...", or "Bearer <token>".
                let key = v.strip_prefix(b"Bearer ").or_else(|| {
                    v.windows(11)
                        .position(|w| w == b"Credential=")
                        .map(|at| &v[at + 11..])
                        .and_then(|rest| rest.split(|b| *b == b'/').next())
                });
                key.is_some_and(|k| {
                    allowed_keys[..allowed_count]
                        .iter()
                        .any(|(a, n)| &a[..*n] == k)
                })
            });
        if let Some(verify) = self.verify {
            authorized &= verify(
                head.method(buf),
                head.target(buf),
                &buf[..head.fields().head_len()],
                head.fields(),
            );
        }
        let (token, token_len) = self.session_token;
        if token_len > 0 {
            authorized &=
                head.fields().header(buf, "x-amz-security-token") == Some(&token[..token_len]);
        }
        authorized
    }
}

/// With the bucket in the path, "/bucket/rest" is served as "/rest".
fn in_bucket<'a>(target: &'a [u8], bucket: &[u8]) -> &'a [u8] {
    target
        .strip_prefix(b"/")
        .and_then(|t| t.strip_prefix(bucket))
        .filter(|rest| !bucket.is_empty() && rest.first() == Some(&b'/'))
        .unwrap_or(target)
}

impl Conn {
    /// Keep the method and target, and what the target and identity headers
    /// ask about the object. Returns whether either is too long to keep.
    fn note_target(&mut self, buf: &[u8], fields: &Fields, method: &[u8], target: &[u8]) -> bool {
        let target_max = if method == b"GET" && target.starts_with(b"/?") {
            LIST_TARGET_MAX
        } else {
            TARGET_MAX
        };
        // A version is named in the query; the object table knows the key alone.
        let (asked, target_len) = identity::Asked::read(|name| fields.header(buf, name), target);
        let target = &target[..target_len];
        let too_long = target.len() > target_max || method.len() > self.method.len();
        self.asked = asked;
        if !too_long {
            self.target[..target.len()].copy_from_slice(target);
            self.target_len = target.len();
            self.method[..method.len()].copy_from_slice(method);
            self.method_len = method.len();
        }
        too_long
    }

    /// Keep what the request's other headers ask for, and forget what the
    /// last response carried.
    fn note_fields(&mut self, buf: &[u8], fields: &Fields) {
        self.close_after = fields.close();
        self.if_none_match = fields.header(buf, "if-none-match") == Some(b"*")
            || fields.header(buf, "x-goog-if-generation-match") == Some(b"0");
        self.claimed = claimed_checksum(buf, fields);
        self.reply_checksum = None;
        self.reply_tag = None;
        self.reply_object = None;
        self.note_describing(&buf[..fields.head_len()]);
        let source = fields
            .header(buf, "x-amz-copy-source")
            .or_else(|| fields.header(buf, "x-goog-copy-source"))
            .filter(|v| v.len() <= TARGET_MAX)
            .unwrap_or(b"");
        self.copy_source[..source.len()].copy_from_slice(source);
        self.copy_source_len = source.len();
        self.checksum_mode = fields.header(buf, "x-amz-checksum-mode") == Some(b"ENABLED");
        self.bearer = fields
            .header(buf, "authorization")
            .is_some_and(|v| v.starts_with(b"Bearer "));
        self.range = fields.header(buf, "range").and_then(|v| {
            let v = core::str::from_utf8(v).ok()?.strip_prefix("bytes=")?;
            let (a, b) = v.split_once('-')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        });
    }
}

/// The checksum the request claims for its body: S3's CRC-64/NVME as sent,
/// or Google Cloud Storage's CRC-32C followed by four zero bytes.
fn claimed_checksum(buf: &[u8], fields: &Fields) -> Option<[u8; 12]> {
    fields
        .header(buf, "x-amz-checksum-crc64nvme")
        .and_then(|v| v.try_into().ok())
        .or_else(|| {
            let v = fields
                .header(buf, "x-goog-hash")?
                .strip_prefix(b"crc32c=")?;
            let mut sum = [0u8; 12];
            sum[..8].copy_from_slice(v.get(..8).filter(|_| v.len() == 8)?);
            Some(sum)
        })
}
