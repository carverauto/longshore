// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The table kernel: one lookup per byte. It runs on any processor, and the
//! accelerated kernels use it for inputs shorter than one block.

use crate::keys::{TABLE32, TABLE64};

/// Advances a CRC-64/NVME state over `data`.
#[inline]
pub(crate) fn crc64(mut state: u64, data: &[u8]) -> u64 {
    for &b in data {
        state = TABLE64[((state as u8) ^ b) as usize] ^ (state >> 8);
    }
    state
}

/// Advances a CRC-32C state over `data`.
#[inline]
pub(crate) fn crc32c(mut state: u32, data: &[u8]) -> u32 {
    for &b in data {
        state = TABLE32[((state as u8) ^ b) as usize] ^ (state >> 8);
    }
    state
}
