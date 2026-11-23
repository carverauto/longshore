// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Every kernel in the build, called directly, against the bit-at-a-time
//! reference. Selection would otherwise leave all but one untested.

mod reference;

use longshore_crc::kernels::{self, Kernel};

/// Deterministic bytes with no short period.
fn bytes(len: usize, mut seed: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 24) as u8
        })
        .collect()
}

fn supported() -> impl Iterator<Item = &'static Kernel> {
    kernels::all().iter().filter(|k| k.supported())
}

/// Every length in `lengths` starting at `data[0]`, for both checksums and
/// two starting states. The reference runs once, a byte at a time, so each
/// length costs one kernel call.
fn every_length(
    kernel: &Kernel,
    data: &[u8],
    lengths: std::ops::RangeInclusive<usize>,
    what: &str,
) {
    let (min, max) = (*lengths.start(), *lengths.end());
    for (s64, s32) in [(!0u64, !0u32), (0x0123_4567_89ab_cdef, 0x89ab_cdef)] {
        let mut want64 = reference::crc64_update(s64, &data[..min]);
        let mut want32 = reference::crc32c_update(s32, &data[..min]);
        for len in min..=max {
            let name = kernel.name();
            assert_eq!(
                kernel.crc64_update(s64, &data[..len]),
                want64,
                "CRC-64/NVME, {name}, length {len}, {what}"
            );
            assert_eq!(
                kernel.crc32c_update(s32, &data[..len]),
                want32,
                "CRC-32C, {name}, length {len}, {what}"
            );
            if len < max {
                want64 = reference::crc64_update(want64, &data[len..len + 1]);
                want32 = reference::crc32c_update(want32, &data[len..len + 1]);
            }
        }
    }
}

#[test]
fn the_selected_kernel_is_supported() {
    let names: Vec<_> = kernels::all()
        .iter()
        .map(|k| {
            format!(
                "{}{}",
                k.name(),
                if k.supported() { "" } else { " (not run)" }
            )
        })
        .collect();
    println!("kernels: {}", names.join(", "));
    println!("selected: {}", kernels::selected().name());
    assert!(kernels::selected().supported());
    assert!(kernels::all().iter().any(|k| k.name() == "table"));
}

#[test]
fn published_check_values() {
    for k in supported() {
        assert_eq!(!k.crc64_update(!0, b"123456789"), 0xae8b_1486_0a79_9888);
        assert_eq!(!k.crc32c_update(!0, b"123456789"), 0xe306_9283);
        // RFC 3720 B.4: 32 bytes of zeros and of ones.
        assert_eq!(!k.crc32c_update(!0, &[0u8; 32]), 0x8a91_36aa);
        assert_eq!(!k.crc32c_update(!0, &[0xffu8; 32]), 0x62a8_ab43);
    }
}

#[test]
fn every_length_to_4096() {
    let data = bytes(4096 + 16, 0x9e37_79b9_7f4a_7c15);
    for k in supported() {
        for align in 0..16 {
            every_length(k, &data[align..], 0..=4096, &format!("offset {align}"));
        }
    }
}

#[test]
fn every_alignment_to_64() {
    let data = bytes(640 + 64, 0x2545_f491_4f6c_dd1d);
    for k in supported() {
        for align in 16..64 {
            every_length(k, &data[align..], 0..=640, &format!("offset {align}"));
        }
    }
}

/// Past 4 KiB some kernels change method for CRC-32C: the CRC instruction
/// reads part of the input beside the multiplier. Every length across the
/// change and for several rounds after it.
#[test]
fn every_length_where_the_method_changes() {
    let data = bytes(8200 + 3, 0x6a09_e667_f3bc_c908);
    for k in supported() {
        for align in [0, 3] {
            every_length(k, &data[align..], 3900..=8200, &format!("offset {align}"));
        }
    }
}

#[test]
fn split_anywhere() {
    let data = bytes(1500, 0xd1b5_4a32_d192_ed03);
    let (whole64, whole32) = (
        reference::crc64_update(!0, &data),
        reference::crc32c_update(!0, &data),
    );
    for k in supported() {
        for cut in 0..=data.len() {
            let (a, b) = data.split_at(cut);
            assert_eq!(k.crc64_update(k.crc64_update(!0, a), b), whole64);
            assert_eq!(k.crc32c_update(k.crc32c_update(!0, a), b), whole32);
        }
    }
}

/// Megabytes at a time, where the fused CRC-32C kernels join their streams
/// with constants no shorter input uses. The table kernel is the reference:
/// the bit-at-a-time one is too slow at this size.
#[test]
fn megabytes_match_the_table_kernel() {
    let table = kernels::all()
        .iter()
        .find(|k| k.name() == "table")
        .expect("the table kernel is in every build");
    let data = bytes((16 << 20) + 5, 0xbb67_ae85_84ca_a73b);
    for len in [(3 << 19) + 3, (16 << 20) + 5] {
        let d = &data[..len];
        let (want64, want32) = (table.crc64_update(!0, d), table.crc32c_update(!0, d));
        for k in supported().filter(|k| k.name() != "table") {
            assert_eq!(k.crc64_update(!0, d), want64, "{} {len}", k.name());
            assert_eq!(k.crc32c_update(!0, d), want32, "{} {len}", k.name());
        }
    }
}

#[test]
fn large_buffers() {
    // Long enough for every stage of every kernel to loop many times, with
    // lengths that leave each kind of remainder.
    let data = bytes(1_000_003, 0x94d0_49bb_1331_11eb);
    for len in [1_000_003, 999_936, 65_536, 65_521, 8_191] {
        let d = &data[..len];
        let (want64, want32) = (reference::crc64_nvme(d), reference::crc32c(d));
        for k in supported() {
            assert_eq!(!k.crc64_update(!0, d), want64, "{} {len}", k.name());
            assert_eq!(!k.crc32c_update(!0, d), want32, "{} {len}", k.name());
        }
    }
}
