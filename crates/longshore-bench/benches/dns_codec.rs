// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! DNS codec microbenchmarks.
//!
//!   cargo bench -p longshore-bench --bench dns_codec
//!   cargo bench -p longshore-bench --bench dns_codec -- --save-baseline main

use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, Throughput};
use longshore::dns::codec::{encode_query, parse_response, MESSAGE_MAX};
use std::hint::black_box;

const NAME: &str = "bucket.us-east-1.linodeobjects.com";

/// A response to the A query for `NAME` carrying `addrs` A records, each with
/// its owner name compressed to the question.
fn response(id: u16, addrs: usize) -> Vec<u8> {
    let mut m = vec![0u8; MESSAGE_MAX];
    let Ok(n) = encode_query(NAME, id, &mut m) else {
        eprintln!("dns_codec: {NAME} does not encode");
        std::process::exit(1)
    };
    m.truncate(n);
    m[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
    m[6..8].copy_from_slice(&(addrs as u16).to_be_bytes());
    for i in 0..addrs {
        m.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 1, 44, 0, 4, 10, 0, 0, i as u8]);
    }
    m
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("dns_codec");

    let _: &mut BenchmarkGroup<'_, WallTime> = g.bench_function("encode_query", |b| {
        let mut out = [0u8; MESSAGE_MAX];
        assert!(encode_query(NAME, 0x1234, &mut out).is_ok());
        b.iter(|| encode_query(black_box(NAME), black_box(0x1234), &mut out))
    });

    for addrs in [1usize, 8] {
        let msg = response(0x1234, addrs);
        assert!(parse_response(&msg, 0x1234, NAME).is_ok());
        let _: &mut BenchmarkGroup<'_, WallTime> =
            g.throughput(Throughput::Bytes(msg.len() as u64));
        let _: &mut BenchmarkGroup<'_, WallTime> = g
            .bench_function(format!("parse_response/{addrs}_addrs"), |b| {
                b.iter(|| parse_response(black_box(&msg), 0x1234, black_box(NAME)))
            });
    }

    // The rejection path matters too: stale and spoofed datagrams take it.
    let msg = response(0x1234, 8);
    assert!(parse_response(&msg, 0x4321, NAME).is_err());
    let _: &mut BenchmarkGroup<'_, WallTime> = g.throughput(Throughput::Bytes(msg.len() as u64));
    let _: &mut BenchmarkGroup<'_, WallTime> = g
        .bench_function("parse_response/id_mismatch", |b| {
            b.iter(|| parse_response(black_box(&msg), 0x4321, black_box(NAME)))
        });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
