// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Operations beyond storing and fetching bytes: asking about an object,
//! storing with optional headers, copying inside the store, erasing many
//! keys at once, listing one level of a hierarchy, and presigned URLs.

use super::*;
use crate::store::presign::{self, Access, PresignError};

impl S3 {
    /// Ask the store about `key` without fetching it. The outcome is
    /// [`Outcome::Stat`], or [`Outcome::NotFound`].
    ///
    /// With `into`, the response head is copied into that buffer so that
    /// headers [`Stat`] has no field for can be read with
    /// [`crate::store::stat::header_in`].
    pub fn stat<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        into: Option<BufId>,
    ) -> Result<(), SubmitError> {
        self.submit(io, token, key, Kind::Stat, None, into, Checksum::None, b"")
    }

    /// What the response on `conn` says about its object.
    pub(super) fn stat_of<IO: Io>(&self, io: &mut IO, into: Option<BufId>, conn: u32) -> Stat {
        Stat {
            size: self.client.content_length(conn).unwrap_or(0),
            modified: self
                .client
                .header(io, conn, "last-modified")
                .and_then(parse_http_date),
            checksum: self.stored_checksum(io, conn),
            tag: self.client.header(io, conn, "etag").and_then(PartTag::new),
            head_len: into.map_or(0, |buf| self.client.copy_head(io, conn, buf)),
        }
    }

    /// [`S3::store`] with optional headers: content type, metadata and so on.
    #[allow(clippy::too_many_arguments)]
    pub fn store_with<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        body: BufId,
        len: u32,
        checksum: Checksum,
        options: &Options,
    ) -> Result<(), SubmitError> {
        let kind = Kind::Store { no_clobber: false };
        self.submit_with(
            io,
            token,
            key,
            kind,
            Some((body, len)),
            checksum,
            b"",
            options,
        )
    }

    /// [`S3::store_no_clobber`] with optional headers.
    #[allow(clippy::too_many_arguments)]
    pub fn store_no_clobber_with<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        body: BufId,
        len: u32,
        checksum: Checksum,
        options: &Options,
    ) -> Result<(), SubmitError> {
        let kind = Kind::Store { no_clobber: true };
        self.submit_with(
            io,
            token,
            key,
            kind,
            Some((body, len)),
            checksum,
            b"",
            options,
        )
    }

    /// [`S3::multipart_begin`] with optional headers. They describe the
    /// object the upload will become.
    pub fn multipart_begin_with<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        checksums: bool,
        options: &Options,
    ) -> Result<(), SubmitError> {
        let checksums = checksums && self.cfg.vendor == Vendor::S3;
        let kind = Kind::Begin { checksums };
        self.submit_with(
            io,
            token,
            key,
            kind,
            None,
            Checksum::None,
            b"uploads=",
            options,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_with<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        kind: Kind,
        body: Option<(BufId, u32)>,
        checksum: Checksum,
        query: &[u8],
        options: &Options,
    ) -> Result<(), SubmitError> {
        let i = self.prepare(io, token, key, kind, body, None, checksum, query)?;
        self.slots[i].options = *options;
        self.attempt(io, i);
        Ok(())
    }

    /// Copy the object at `from_key` to `to_key` inside the store. No bytes
    /// pass through the client. The outcome is [`Outcome::Copied`], or
    /// [`Outcome::NotFound`] if there is nothing at `from_key`.
    pub fn copy<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        from_key: &str,
        to_key: &str,
    ) -> Result<(), SubmitError> {
        if from_key.is_empty() {
            return Err(SubmitError::BadKey);
        }
        // The source is named as "/bucket/key". The bucket is the one in
        // the path, or else the first label of the host.
        let mut source = [0u8; QUERY_MAX];
        let bucket = match &self.cfg.bucket_in_path {
            Some(bucket) => bucket.as_bytes(),
            None => self.cfg.host.split('.').next().unwrap_or("").as_bytes(),
        };
        let key_at = bucket.len() + 2;
        if key_at >= source.len() {
            return Err(SubmitError::BadKey);
        }
        source[0] = b'/';
        source[1..key_at - 1].copy_from_slice(bucket);
        source[key_at - 1] = b'/';
        let n = key_at
            + uri_encode(from_key.as_bytes(), false, &mut source[key_at..])
                .ok_or(SubmitError::BadKey)?;
        self.submit(
            io,
            token,
            to_key,
            Kind::Copy,
            None,
            None,
            Checksum::None,
            &source[..n],
        )
    }

    /// Erase the keys named in `manifest`, `len` bytes written by
    /// [`crate::store::erase::write_manifest`]: up to 1000 in one request.
    /// The outcome is [`Outcome::ErasedMany`] with the store's report in
    /// `into`. A key that was not there counts as erased.
    ///
    /// Google Cloud Storage has no such request: [`SubmitError::Unsupported`].
    pub fn erase_many<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        manifest: BufId,
        len: u32,
        into: BufId,
    ) -> Result<(), SubmitError> {
        if self.cfg.vendor != Vendor::S3 {
            return Err(SubmitError::Unsupported);
        }
        let i = self.slots[..self.cfg.requests_max as usize]
            .iter()
            .position(|s| s.state == State::Free)
            .ok_or(SubmitError::Busy)?;
        // The store requires an integrity header on this request.
        let crc = crate::checksum::crc64_nvme(&io.buf(manifest)[..len as usize]);
        let target = b"/?delete=";
        let at = self.root.len();
        let s = &mut self.slots[i];
        s.path[..at].copy_from_slice(self.root.as_bytes());
        s.path[at..at + target.len()].copy_from_slice(target);
        s.path_len = at + target.len();
        s.query_len = 0;
        s.options = Options::new();
        s.token = token;
        s.kind = Kind::EraseMany;
        s.body = Some((manifest, len));
        s.response_buf = Some(into);
        (s.candidate, s.comparing) = (false, false);
        (s.attempts, s.delays, s.address_waits, s.crc) = (0, 0, 0, Some(crc));
        self.attempt(io, i);
        Ok(())
    }

    /// [`S3::list`] with a delimiter. Keys that contain the delimiter after
    /// `prefix` are not listed themselves: the listing reports the part of
    /// each up to and including the delimiter once, among its
    /// [`common prefixes`](crate::store::list::Listing::common_prefixes).
    /// With `/` this lists one level of a hierarchy.
    ///
    /// To page, use [`S3::list_continue`] with the token the truncated page
    /// returned. Paging by `start_after` does not work with a delimiter when
    /// a page ends on a common prefix: no key skips everything under a
    /// prefix, so the prefix would be reported again.
    #[allow(clippy::too_many_arguments)]
    pub fn list_with<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        prefix: &str,
        start_after: Option<&str>,
        max_keys: u16,
        delimiter: Option<&str>,
        into: BufId,
    ) -> Result<(), SubmitError> {
        let i = self.slots[..self.cfg.requests_max as usize]
            .iter()
            .position(|s| s.state == State::Free)
            .ok_or(SubmitError::Busy)?;
        self.slots[i].candidate = false;
        self.slots[i].comparing = false;
        let from = start_after.map_or(submit::ListFrom::Start, submit::ListFrom::After);
        self.list_in_slot(io, i, token, prefix, from, max_keys, delimiter, into)
    }

    /// The next page of a truncated listing. `continuation` is the token
    /// that page returned
    /// ([`Listing::next_token_raw`](crate::store::list::Listing::next_token_raw),
    /// decoded with [`unescape`](crate::store::list::unescape)). `prefix`,
    /// `max_keys` and `delimiter` must be what the listing was started with.
    /// A token longer than [`LIST_TOKEN_ENCODED_MAX`] once encoded is
    /// refused as [`SubmitError::BadKey`].
    #[allow(clippy::too_many_arguments)]
    pub fn list_continue<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        prefix: &str,
        continuation: &str,
        max_keys: u16,
        delimiter: Option<&str>,
        into: BufId,
    ) -> Result<(), SubmitError> {
        if continuation.is_empty() {
            return Err(SubmitError::BadKey);
        }
        let i = self.slots[..self.cfg.requests_max as usize]
            .iter()
            .position(|s| s.state == State::Free)
            .ok_or(SubmitError::Busy)?;
        self.slots[i].candidate = false;
        self.slots[i].comparing = false;
        let from = submit::ListFrom::Token(continuation);
        self.list_in_slot(io, i, token, prefix, from, max_keys, delimiter, into)
    }

    /// Write a presigned URL for `key` into `out` and return its length:
    /// a URL whose holder may fetch or store that one object for
    /// `expires_secs`, signed with the credentials in use now. Nothing is
    /// sent. See [`crate::store::presign`].
    pub fn presign<IO: Io>(
        &self,
        io: &mut IO,
        out: &mut [u8],
        access: Access,
        key: &str,
        expires_secs: u32,
    ) -> Result<usize, PresignError> {
        if self.cfg.vendor != Vendor::S3 {
            return Err(PresignError::NotSigningCredentials);
        }
        let default_port = if self.secure { 443 } else { 80 };
        let mut authority = [0u8; 253 + 6];
        let host = self.cfg.host.as_bytes();
        let mut n = host.len();
        if n > 253 {
            return Err(PresignError::OutputTooSmall);
        }
        authority[..n].copy_from_slice(host);
        if self.cfg.port != default_port {
            authority[n] = b':';
            n += 1;
            let mut digits = [0u8; 5];
            let mut port = self.cfg.port;
            let mut d = digits.len();
            loop {
                d -= 1;
                digits[d] = b'0' + (port % 10) as u8;
                port /= 10;
                if port == 0 {
                    break;
                }
            }
            authority[n..n + digits.len() - d].copy_from_slice(&digits[d..]);
            n += digits.len() - d;
        }
        let target = presign::Target {
            scheme: if self.secure { "https" } else { "http" },
            host: core::str::from_utf8(&authority[..n]).expect("a host and digits"),
            root: &self.root,
            region: &self.cfg.region,
            credentials: &self.creds[self.active],
        };
        presign::presign(out, &target, access, key, io.unix_secs(), expires_secs)
    }
}
