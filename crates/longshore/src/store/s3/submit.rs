// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Starting operations: one slot per store, fetch, list or erase.

use super::*;

impl S3 {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn submit<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        kind: Kind,
        body: Option<(BufId, u32)>,
        response_buf: Option<BufId>,
        checksum: Checksum,
    ) -> Result<(), SubmitError> {
        let i = self.slots[..self.cfg.requests_max as usize]
            .iter()
            .position(|s| s.state == State::Free)
            .ok_or(SubmitError::Busy)?;
        let s = &mut self.slots[i];
        s.candidate = false;
        s.comparing = false;
        if key.is_empty() {
            return Err(SubmitError::BadKey);
        }
        s.path[0] = b'/';
        s.path_len =
            1 + uri_encode(key.as_bytes(), false, &mut s.path[1..]).ok_or(SubmitError::BadKey)?;
        s.token = token;
        s.kind = kind;
        s.body = body;
        s.response_buf = response_buf;
        s.attempts = 0;
        s.address_waits = 0;
        s.delays = 0;
        s.crc = match (checksum, body) {
            (Checksum::Provided(crc), _) => Some(crc),
            (Checksum::Compute, Some((buf, len))) => Some(match self.cfg.vendor {
                Vendor::S3 => crate::checksum::crc64_nvme(&io.buf(buf)[..len as usize]),
                Vendor::Gcs => crate::checksum::crc32c(&io.buf(buf)[..len as usize]) as u64,
            }),
            _ => None,
        };
        self.attempt(io, i);
        Ok(())
    }

    /// Store `len` bytes of `body` at `key`, replacing any existing object.
    pub fn store<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        body: BufId,
        len: u32,
        checksum: Checksum,
    ) -> Result<(), SubmitError> {
        self.submit(
            io,
            token,
            key,
            Kind::Store { no_clobber: false },
            Some((body, len)),
            None,
            checksum,
        )
    }

    /// Store only if no object exists at `key`.
    pub fn store_no_clobber<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        body: BufId,
        len: u32,
        checksum: Checksum,
    ) -> Result<(), SubmitError> {
        self.submit(
            io,
            token,
            key,
            Kind::Store { no_clobber: true },
            Some((body, len)),
            None,
            checksum,
        )
    }

    /// Fetch the object, or the inclusive byte `range` of it, into `into`.
    pub fn fetch<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        range: Option<(u64, u64)>,
        into: BufId,
    ) -> Result<(), SubmitError> {
        if range.is_some_and(|(first, last)| first > last) {
            return Err(SubmitError::BadRange);
        }
        self.submit(
            io,
            token,
            key,
            Kind::Fetch { range },
            None,
            Some(into),
            Checksum::None,
        )
    }

    /// List up to `max_keys` keys that start with `prefix` and sort after
    /// `start_after`, into `into`. To page, pass the last key of the previous
    /// page as `start_after` while the listing reports `is_truncated`.
    pub fn list<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        prefix: &str,
        start_after: Option<&str>,
        max_keys: u16,
        into: BufId,
    ) -> Result<(), SubmitError> {
        let i = self.slots[..self.cfg.requests_max as usize]
            .iter()
            .position(|s| s.state == State::Free)
            .ok_or(SubmitError::Busy)?;
        self.slots[i].candidate = false;
        self.slots[i].comparing = false;
        self.list_in_slot(io, i, token, prefix, start_after, max_keys, into)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn list_in_slot<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        token: u64,
        prefix: &str,
        start_after: Option<&str>,
        max_keys: u16,
        into: BufId,
    ) -> Result<(), SubmitError> {
        // The query is stored already encoded and in the sorted order
        // signing requires: list-type, max-keys, prefix, start-after.
        let s = &mut self.slots[i];
        let mut n = 0;
        let mut put = |path: &mut [u8], bytes: &[u8]| -> Option<()> {
            path.get_mut(n..n + bytes.len())?.copy_from_slice(bytes);
            n += bytes.len();
            Some(())
        };
        let mut digits = [0u8; 5];
        let mut d = digits.len();
        let mut v = max_keys.max(1);
        while v > 0 {
            d -= 1;
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        let built = (|| {
            put(&mut s.path, b"/?list-type=2&max-keys=")?;
            put(&mut s.path, &digits[d..])?;
            for (name, value) in [
                ("&prefix=", Some(prefix).filter(|p| !p.is_empty())),
                ("&start-after=", start_after),
            ] {
                if let Some(value) = value {
                    put(&mut s.path, name.as_bytes())?;
                    let mut enc = [0u8; KEY_ENCODED_MAX];
                    let len = uri_encode(value.as_bytes(), true, &mut enc)?;
                    put(&mut s.path, &enc[..len])?;
                }
            }
            Some(())
        })();
        if built.is_none() {
            return Err(SubmitError::BadKey);
        }
        s.path_len = n;
        s.token = token;
        s.kind = Kind::List;
        s.body = None;
        s.response_buf = Some(into);
        (s.attempts, s.delays, s.address_waits, s.crc) = (0, 0, 0, None);
        self.attempt(io, i);
        Ok(())
    }

    pub fn erase<IO: Io>(&mut self, io: &mut IO, token: u64, key: &str) -> Result<(), SubmitError> {
        self.submit(io, token, key, Kind::Erase, None, None, Checksum::None)
    }
}
