// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Checksums: the form the store's headers carry them in, the one the store
//! sent with a response, and the check of a body against it.

use super::*;

impl S3 {
    /// Check a fetched body against the store's checksum, and read a refusal
    /// of the checksum we sent: either makes the attempt one to retry.
    pub(super) fn check_body<IO: Io>(
        &self,
        io: &mut IO,
        s: &Slot,
        result: Result<client::Response, Error>,
        class: Class,
    ) -> Checked {
        let mut checked = Checked {
            class,
            verified: false,
            mismatch: false,
        };
        match (result, s.kind) {
            // A whole-object fetch: check the bytes against the store's checksum.
            (Ok(r), Kind::Fetch { range: None }) if class == Class::Success => {
                if let Some(expected) = self.stored_checksum(io, r.conn()) {
                    #[expect(
                        clippy::expect_used,
                        reason = "a fetch is always given a response buffer"
                    )]
                    let buf = s.response_buf.expect("fetches have a response buffer");
                    let body = &io.buf(buf)[..r.body_len() as usize];
                    let actual = self.checksum_value(match self.cfg.vendor {
                        Vendor::S3 => longshore_crc::crc64_nvme(body),
                        Vendor::Gcs => u64::from(longshore_crc::crc32c(body)),
                    });
                    checked.verified = actual == expected;
                    // Corrupted in transit: fetch again rather than hand back bad bytes.
                    if !checked.verified {
                        checked.mismatch = true;
                        checked.class = Class::Retry;
                    }
                } else if self.cfg.require_checksums {
                    // The checksum itself is missing or mangled.
                    checked.mismatch = true;
                    checked.class = Class::Retry;
                }
            }
            // The store rejected our checksum: the body was damaged on the way.
            (Ok(r), kind)
                if (kind.stores_bytes() || kind == Kind::EraseMany)
                    && r.status() == 400
                    && s.crc.is_some()
                    && !s.comparing =>
            {
                // A batch erase reads its response into the caller's buffer.
                let doc = match s.response_buf.filter(|_| kind == Kind::EraseMany) {
                    Some(buf) => &io.buf(buf)[..r.body_len() as usize],
                    None => self.client.discarded_body(io, r.conn()),
                };
                // S3 names it BadDigest (assumed; no enforcing S3 store has been
                // tried). Google Cloud Storage names it CrcMismatch (observed).
                let code: &[u8] = match self.cfg.vendor {
                    Vendor::S3 => b"BadDigest",
                    Vendor::Gcs => b"CrcMismatch",
                };
                if doc.windows(code.len()).any(|w| w == code) {
                    checked.class = Class::Retry;
                }
            }
            _ => {}
        }
        checked
    }

    /// A checksum as the store's header carries it, padded with zeros:
    /// twelve base64 characters for S3, eight for Google Cloud Storage.
    pub(super) fn checksum_value(&self, crc: u64) -> [u8; 12] {
        match self.cfg.vendor {
            Vendor::S3 => longshore_crc::crc64_header_value(crc),
            Vendor::Gcs => {
                let mut out = [0u8; 12];
                out[..8].copy_from_slice(&longshore_crc::crc32c_header_value(crc as u32));
                out
            }
        }
    }

    /// The checksum the store sent with the response on `conn`, in the form
    /// [`S3::checksum_value`] produces. `None` if there is none or it is malformed.
    pub(super) fn stored_checksum<IO: Io>(&self, io: &mut IO, conn: u32) -> Option<[u8; 12]> {
        let mut out = [0u8; 12];
        match self.cfg.vendor {
            Vendor::S3 => out.copy_from_slice(
                self.client
                    .header(io, conn, CHECKSUM_HEADER)
                    .filter(|v| v.len() == 12)?,
            ),
            Vendor::Gcs => {
                // "crc32c=AAAAAA==,md5=...": the parts may come in either
                // order, on one line or on a line each.
                let part = self
                    .client
                    .headers(io, conn, "x-goog-hash")
                    .flat_map(|value| value.split(|b| *b == b','))
                    .map(|p| p.trim_ascii())
                    .find_map(|p| p.strip_prefix(b"crc32c="))?;
                out[..8].copy_from_slice(Some(part).filter(|p| p.len() == 8)?);
            }
        }
        Some(out)
    }
}

/// An attempt's class after its body was checked, and what the check found.
#[derive(Clone, Copy)]
pub(super) struct Checked {
    pub(super) class: Class,
    /// The fetched bytes match the store's checksum.
    pub(super) verified: bool,
    /// They do not, or the checksum the store must send is missing.
    pub(super) mismatch: bool,
}
