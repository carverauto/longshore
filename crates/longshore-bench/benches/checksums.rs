// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Candidate checksum libraries, compared through one harness.
//!
//! Reference from the talk: 4.3 GB/s for CRC64 on one core. Every candidate
//! is first checked against the algorithm's published check value, so a fast
//! wrong answer cannot win.

use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, Throughput};
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
fn crc64_longshore(data: &[u8]) -> u64 {
    longshore::checksum::crc64_nvme(data)
}
fn crc32c_longshore(data: &[u8]) -> u32 {
    longshore::checksum::crc32c(data)
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

/// Every kernel this processor supports against the table kernel: each
/// length to 8,192 bytes at 4 offsets, and one long odd-sized buffer. The
/// crate's tests do the same, but they run where they were built; this runs
/// on the benchmark machine, which may have instructions that one lacks.
fn verify_kernels() {
    let all = longshore::checksum::kernels::all();
    let table = all.iter().find(|k| k.name == "table").unwrap();
    let data: Vec<u8> = (0..1_000_003u64)
        .map(|i| (i.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 56) as u8)
        .collect();
    assert_eq!(!table.crc64_update(!0, &data), crc64_table(&data));
    assert_eq!(!table.crc32c_update(!0, &data), crc32c_table(&data));
    for k in all.iter().filter(|k| k.supported()) {
        for offset in [0, 1, 7, 15] {
            let d = &data[offset..];
            let (mut want64, mut want32) = (!0u64, !0u32);
            for len in 0..=8192 {
                let name = k.name;
                assert_eq!(k.crc64_update(!0, &d[..len]), want64, "{name} {len}");
                assert_eq!(k.crc32c_update(!0, &d[..len]), want32, "{name} {len}");
                want64 = table.crc64_update(want64, &d[len..len + 1]);
                want32 = table.crc32c_update(want32, &d[len..len + 1]);
            }
        }
        assert_eq!(!k.crc64_update(!0, &data), crc64_table(&data), "{}", k.name);
        assert_eq!(
            !k.crc32c_update(!0, &data),
            crc32c_table(&data),
            "{}",
            k.name
        );
        println!("verified: {}", k.name);
    }
}

fn bench(c: &mut Criterion) {
    verify_kernels();
    type F64 = fn(&[u8]) -> u64;
    type F32 = fn(&[u8]) -> u32;
    let crc64: [(&str, F64); 4] = [
        ("crc (table)", crc64_table),
        ("crc64fast-nvme", crc64_crc64fast_nvme),
        ("crc-fast", crc64_crc_fast),
        ("longshore", crc64_longshore),
    ];
    let crc32c: [(&str, F32); 4] = [
        ("crc (table)", crc32c_table),
        ("crc32c", crc32c_crc32c),
        ("crc-fast", crc32c_crc_fast),
        ("longshore", crc32c_longshore),
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

    // Each of longshore's kernels this processor supports, by name, so lane
    // counts and register widths can be compared. "longshore" above is the
    // one it selects.
    let kernels: Vec<_> = longshore::checksum::kernels::all()
        .iter()
        .filter(|k| k.supported() && k.name != "table")
        .collect();
    println!(
        "longshore selects: {}",
        longshore::checksum::kernels::selected().name
    );

    // 64 B, 256 B and 1 KiB show the cost of a call and of the stages that
    // handle what the main loop leaves; 64 KiB is the zero-copy
    // threshold; 8 MiB is the talk's object size and does not fit in L2, so
    // memory bandwidth shows up.
    for size in [64usize, 256, 1024, 64 * 1024, 8 << 20] {
        let data: Vec<u8> = (0..size)
            .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1])
            .collect();
        let label = if size >= 1 << 20 {
            format!("{}MiB", size >> 20)
        } else if size >= 1 << 10 {
            format!("{}KiB", size >> 10)
        } else {
            format!("{size}B")
        };
        let mut g64 = c.benchmark_group(format!("crc64_nvme/{label}"));
        let _: &mut BenchmarkGroup<'_, WallTime> = g64.throughput(Throughput::Bytes(size as u64));
        for (name, f) in crc64 {
            let _: &mut BenchmarkGroup<'_, WallTime> =
                g64.bench_function(name, |b| b.iter(|| f(black_box(&data))));
        }
        for k in &kernels {
            let _: &mut BenchmarkGroup<'_, WallTime> = g64
                .bench_function(format!("longshore {}", k.name), |b| {
                    b.iter(|| !k.crc64_update(!0, black_box(&data)))
                });
        }
        g64.finish();
        let mut g32 = c.benchmark_group(format!("crc32c/{label}"));
        let _: &mut BenchmarkGroup<'_, WallTime> = g32.throughput(Throughput::Bytes(size as u64));
        for (name, f) in crc32c {
            let _: &mut BenchmarkGroup<'_, WallTime> =
                g32.bench_function(name, |b| b.iter(|| f(black_box(&data))));
        }
        for k in &kernels {
            let _: &mut BenchmarkGroup<'_, WallTime> = g32
                .bench_function(format!("longshore {}", k.name), |b| {
                    b.iter(|| !k.crc32c_update(!0, black_box(&data)))
                });
        }
        g32.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
