// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) and an allocation check for the checksum wrappers.

use hegel::generators as gs;
use hegel::TestCase;
use osc_checksum::{base64, crc32c, crc64_nvme, Crc32c, Crc64Nvme};
use osc_io::alloc_guard::{sealed, Guard};

#[global_allocator]
static ALLOC: Guard = Guard;

/// Bit-at-a-time reference implementations, straight from the definitions.
fn crc64_nvme_reference(data: &[u8]) -> u64 {
    let mut crc = !0u64;
    for &b in data {
        crc ^= b as u64;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x9a6c_9329_ac4b_c9b5
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn crc32c_reference(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x82f6_3b78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The accelerated library agrees with an independent bitwise implementation
/// for every input, at every length and alignment.
#[hegel::test]
fn accelerated_matches_bitwise_reference(tc: TestCase) {
    let data = tc.draw(gs::binary().max_size(700));
    let skip = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(data.len().min(15)),
    );
    let slice = &data[skip..];
    assert_eq!(crc64_nvme(slice), crc64_nvme_reference(slice));
    assert_eq!(crc32c(slice), crc32c_reference(slice));
}

/// Splitting the input anywhere does not change the result.
#[hegel::test]
fn incremental_equals_one_shot(tc: TestCase) {
    let data = tc.draw(gs::binary().max_size(700));
    let cut = tc.draw(gs::integers::<usize>().min_value(0).max_value(data.len()));
    let (mut a, mut b) = (Crc64Nvme::new(), Crc32c::new());
    a.update(&data[..cut]);
    a.update(&data[cut..]);
    b.update(&data[..cut]);
    b.update(&data[cut..]);
    assert_eq!((a.finish(), b.finish()), (crc64_nvme(&data), crc32c(&data)));
}

/// Any single flipped bit changes both checksums.
#[hegel::test]
fn a_flipped_bit_is_detected(tc: TestCase) {
    let mut data = tc.draw(gs::binary().min_size(1).max_size(700));
    let (before64, before32) = (crc64_nvme(&data), crc32c(&data));
    let bit = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(data.len() * 8 - 1),
    );
    data[bit / 8] ^= 1 << (bit % 8);
    assert_ne!(crc64_nvme(&data), before64);
    assert_ne!(crc32c(&data), before32);
}

#[test]
fn large_buffers_match_the_reference() {
    // Long enough to exercise every folding path of the accelerated code.
    let data: Vec<u8> = (0..1_000_003u32)
        .map(|i| i.wrapping_mul(2654435761).to_le_bytes()[1])
        .collect();
    assert_eq!(crc64_nvme(&data), crc64_nvme_reference(&data));
    assert_eq!(crc32c(&data), crc32c_reference(&data));
}

#[test]
fn checksumming_does_not_allocate() {
    let data = vec![0xa5u8; 1 << 20];
    let mut out = [0u8; 16];
    let r = sealed(|| {
        let mut d = Crc64Nvme::new();
        d.update(&data[..1000]);
        d.update(&data[1000..]);
        let n = base64(&d.finish().to_be_bytes(), &mut out);
        (d.finish(), crc32c(&data), n)
    });
    assert_eq!((r.0, r.2), (crc64_nvme(&data), 12));
    assert_eq!(r.1, crc32c_reference(&data));
}
