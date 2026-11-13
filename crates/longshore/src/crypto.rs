// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Cryptographic primitives behind in-tree interfaces.
//!
//! The rest of the workspace calls these functions and never names the
//! library underneath, so a primitive can be swapped for another library or
//! an in-tree implementation without touching callers. None of these
//! allocate.

use ring::{digest, hmac};

pub const SHA256_LEN: usize = 32;

/// Incremental SHA-256.
pub struct Sha256(digest::Context);

impl Sha256 {
    pub fn new() -> Sha256 {
        Sha256(digest::Context::new(&digest::SHA256))
    }
    pub fn update(&mut self, data: &[u8]) -> &mut Sha256 {
        self.0.update(data);
        self
    }
    pub fn finish(self) -> [u8; SHA256_LEN] {
        self.0
            .finish()
            .as_ref()
            .try_into()
            .expect("SHA-256 output is 32 bytes")
    }
}

impl Default for Sha256 {
    fn default() -> Sha256 {
        Sha256::new()
    }
}

pub fn sha256(data: &[u8]) -> [u8; SHA256_LEN] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

/// HMAC-SHA-256 of `data` under `key`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; SHA256_LEN] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&key, data)
        .as_ref()
        .try_into()
        .expect("HMAC-SHA-256 output is 32 bytes")
}

/// Lowercase hex of `bytes` into `out`, which must be twice as long.
pub fn hex(bytes: &[u8], out: &mut [u8]) {
    assert_eq!(out.len(), bytes.len() * 2);
    for (i, b) in bytes.iter().enumerate() {
        out[i * 2] = b"0123456789abcdef"[(b >> 4) as usize];
        out[i * 2 + 1] = b"0123456789abcdef"[(b & 15) as usize];
    }
}

#[cfg(test)]
mod tests;
