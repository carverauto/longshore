// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Candidate checksum libraries, compared through one harness.
//!
//! Reference from the talk: 4.3 GB/s for CRC64 on one core. Every candidate
//! is first checked against the algorithm's published check value, so a fast
//! wrong answer cannot win.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;

const CHECK_INPUT: &[u8] = b"123456789";
const CRC64_NVME_CHECK: u64 = 0xae8b_1486_0a79_9888;
const CRC32C_CHECK: u32 = 0xe306_9283;

fn crc64_table(data: &[u8]) -> u64 {
    crc::Crc::<u64>::new(&crc::CRC_64_NVME).checksum(data)
}
fn crc64_crc64fast_nvme(data: &[u8]) -> u64 {
    let mut d = crc64fast_nvme::Digest::new();
    d.write(data);
    d.sum64()
}
fn crc64_crc_fast(data: &[u8]) -> u64 {
    let mut d = crc_fast::Digest::new(crc_fast::CrcAlgorithm::Crc64Nvme);
    d.update(data);
    d.finalize()
}
fn crc32c_table(data: &[u8]) -> u32 {
    crc::Crc::<u32>::new(&crc::CRC_32_ISCSI).checksum(data)
}
fn crc32c_crc32c(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}
fn crc32c_crc_fast(data: &[u8]) -> u32 {
    let mut d = crc_fast::Digest::new(crc_fast::CrcAlgorithm::Crc32Iscsi);
    d.update(data);
    d.finalize() as u32
}

fn bench(c: &mut Criterion) {
    type F64 = fn(&[u8]) -> u64;
    type F32 = fn(&[u8]) -> u32;
    let crc64: [(&str, F64); 3] = [
        ("crc (table)", crc64_table),
        ("crc64fast-nvme", crc64_crc64fast_nvme),
        ("crc-fast", crc64_crc_fast),
    ];
    let crc32c: [(&str, F32); 3] = [
        ("crc (table)", crc32c_table),
        ("crc32c", crc32c_crc32c),
        ("crc-fast", crc32c_crc_fast),
    ];
    for (name, f) in crc64 {
        assert_eq!(
            f(CHECK_INPUT),
            CRC64_NVME_CHECK,
            "{name} fails the CRC-64/NVME check value"
        );
    }
    for (name, f) in crc32c {
        assert_eq!(
            f(CHECK_INPUT),
            CRC32C_CHECK,
            "{name} fails the CRC-32C check value"
        );
    }

    // 64 KiB is the zero-copy threshold; 8 MiB is the talk's object size and
    // does not fit in L2, so memory bandwidth shows up.
    for size in [64 * 1024usize, 8 << 20] {
        let data: Vec<u8> = (0..size)
            .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1])
            .collect();
        let label = if size >= 1 << 20 {
            format!("{}MiB", size >> 20)
        } else {
            format!("{}KiB", size >> 10)
        };
        let mut g = c.benchmark_group(format!("crc64_nvme/{label}"));
        g.throughput(Throughput::Bytes(size as u64));
        for (name, f) in crc64 {
            g.bench_function(name, |b| b.iter(|| f(black_box(&data))));
        }
        g.finish();
        let mut g = c.benchmark_group(format!("crc32c/{label}"));
        g.throughput(Throughput::Bytes(size as u64));
        for (name, f) in crc32c {
            g.bench_function(name, |b| b.iter(|| f(black_box(&data))));
        }
        g.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
