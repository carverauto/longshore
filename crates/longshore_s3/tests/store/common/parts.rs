// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the multipart test files: beginning an upload, storing parts
//! and writing their manifest.

use super::*;
use longshore_s3::multipart::{write_manifest, Part, UploadId};

pub const PART: u32 = 4000;

/// The bytes part `number` is filled with.
pub fn part_bytes(number: u32) -> Vec<u8> {
    (0..PART - number)
        .map(|i| i.wrapping_mul(2654435761).to_le_bytes()[1] ^ number as u8)
        .collect()
}

impl World {
    pub fn begin(&mut self, token: u64, key: &str, checksums: bool) -> UploadId {
        self.s3
            .multipart_begin(&mut self.io, token, key, checksums)
            .unwrap();
        match self.last().outcome {
            Outcome::Begun { upload } => upload,
            other @ (Outcome::Stored
            | Outcome::AlreadyExists { .. }
            | Outcome::Fetched { .. }
            | Outcome::Listed { .. }
            | Outcome::NotFound
            | Outcome::Erased
            | Outcome::PartStored { .. }
            | Outcome::Aborted
            | Outcome::Stat(_)
            | Outcome::Copied
            | Outcome::ErasedMany { .. }
            | Outcome::Report(_)
            | Outcome::Fatal(_)) => panic!("expected an upload, got {other:?}"),
        }
    }

    /// Submit part `number` from `buf` without waiting for it.
    pub fn send_part(&mut self, token: u64, key: &str, upload: &UploadId, number: u32, buf: BufId) {
        let bytes = part_bytes(number);
        self.io.buf(buf)[..bytes.len()].copy_from_slice(&bytes);
        self.s3
            .multipart_store_part(
                &mut self.io,
                token,
                key,
                upload,
                number,
                buf,
                bytes.len() as u32,
                Checksum::Compute,
            )
            .unwrap();
    }

    /// Part `number` as completion lists it, from the outcome of `token`.
    pub fn part_of(&self, number: u32, token: u64) -> Part {
        match self
            .done
            .iter()
            .find(|d| d.token == token)
            .map(|d| d.outcome)
        {
            Some(Outcome::PartStored { tag, checksum }) => {
                // Every part here is sent with a computed CRC-64/NVME.
                let crc = longshore_crc::crc64_nvme(&part_bytes(number));
                assert_eq!(checksum, Some(crc));
                Part {
                    number,
                    tag,
                    checksum: checksum.map(longshore_crc::crc64_header_value),
                }
            }
            other => panic!("expected a stored part for token {token}, got {other:?}"),
        }
    }

    /// Store parts 1 to 3 and write their manifest into `TX`. Returns its
    /// length and the whole object.
    pub fn three_parts(&mut self, key: &str, upload: &UploadId) -> (u32, Vec<u8>) {
        // Part 2 first, then 1 and 3 at the same time from two buffers.
        self.send_part(12, key, upload, 2, TX);
        let _: longshore_s3::s3::Done = self.last();
        self.send_part(11, key, upload, 1, TX);
        self.send_part(13, key, upload, 3, RX);
        self.run_until_done(self.done.len() + 2);
        let parts = [
            self.part_of(1, 11),
            self.part_of(2, 12),
            self.part_of(3, 13),
        ];
        let len = write_manifest(self.io.buf(TX), &parts).unwrap() as u32;
        let object = [part_bytes(1), part_bytes(2), part_bytes(3)].concat();
        (len, object)
    }
}
