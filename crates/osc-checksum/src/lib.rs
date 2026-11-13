// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Integrity checksums behind in-tree interfaces: CRC-64/NVME (AWS, Azure)
//! and CRC-32C (Google Cloud Storage). Neither allocates.

use crc_fast::{CrcAlgorithm, Digest};

/// CRC-64/NVME of `data`.
pub fn crc64_nvme(data: &[u8]) -> u64 {
    let mut d = Crc64Nvme::new();
    d.update(data);
    d.finish()
}

/// CRC-32C (Castagnoli) of `data`.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut d = Crc32c::new();
    d.update(data);
    d.finish()
}

/// Incremental CRC-64/NVME, for data that arrives in pieces.
pub struct Crc64Nvme(Digest);

impl Crc64Nvme {
    pub fn new() -> Crc64Nvme {
        Crc64Nvme(Digest::new(CrcAlgorithm::Crc64Nvme))
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finish(&self) -> u64 {
        self.0.finalize()
    }
}

impl Default for Crc64Nvme {
    fn default() -> Self {
        Self::new()
    }
}

/// Incremental CRC-32C.
pub struct Crc32c(Digest);

impl Crc32c {
    pub fn new() -> Crc32c {
        Crc32c(Digest::new(CrcAlgorithm::Crc32Iscsi))
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finish(&self) -> u32 {
        self.0.finalize() as u32
    }
}

impl Default for Crc32c {
    fn default() -> Self {
        Self::new()
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. `out` must hold `4 * ceil(len / 3)` bytes.
/// Returns the encoded length.
pub fn base64(input: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out[n] = B64[(b[0] >> 2) as usize];
        out[n + 1] = B64[((b[0] & 3) << 4 | b[1] >> 4) as usize];
        out[n + 2] = if chunk.len() > 1 {
            B64[((b[1] & 15) << 2 | b[2] >> 6) as usize]
        } else {
            b'='
        };
        out[n + 3] = if chunk.len() > 2 {
            B64[(b[2] & 63) as usize]
        } else {
            b'='
        };
        n += 4;
    }
    n
}

/// The value S3 expects in `x-amz-checksum-crc64nvme`: base64 of the
/// checksum's eight bytes, most significant first.
pub fn crc64_header_value(crc: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    base64(&crc.to_be_bytes(), &mut out);
    out
}

/// The value Azure Blob Storage expects in `x-ms-content-crc64`: the same
/// CRC-64/NVME, but least significant byte first. Established against the
/// Azurite emulator, which accepts this order and answers `Crc64Mismatch`
/// to the other; not yet against a real storage account.
pub fn crc64_azure_header_value(crc: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    base64(&crc.to_le_bytes(), &mut out);
    out
}

/// The value Google Cloud Storage expects for CRC-32C: base64 of four bytes,
/// most significant first.
pub fn crc32c_header_value(crc: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    base64(&crc.to_be_bytes(), &mut out);
    out
}

#[cfg(test)]
mod tests;
