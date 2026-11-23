// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Bit-at-a-time reference implementations, straight from the definitions.
//! They take and return the raw state, so a test can start anywhere.

pub fn crc64_update(mut crc: u64, data: &[u8]) -> u64 {
    for &b in data {
        crc ^= u64::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x9a6c_9329_ac4b_c9b5
            } else {
                crc >> 1
            };
        }
    }
    crc
}

pub fn crc32c_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x82f6_3b78
            } else {
                crc >> 1
            };
        }
    }
    crc
}

pub fn crc64_nvme(data: &[u8]) -> u64 {
    !crc64_update(!0, data)
}

pub fn crc32c(data: &[u8]) -> u32 {
    !crc32c_update(!0, data)
}
