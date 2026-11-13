// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! DNS codec microbenchmarks.
//!
//!   cargo bench -p osc-bench --bench dns_codec
//!   cargo bench -p osc-bench --bench dns_codec -- --save-baseline main

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use osc_dns::codec::{encode_query, parse_response, MESSAGE_MAX};
use std::hint::black_box;

const NAME: &str = "bucket.us-east-1.linodeobjects.com";

/// A response to the A query for `NAME` carrying `addrs` A records, each with
/// its owner name compressed to the question.
fn response(id: u16, addrs: usize) -> Vec<u8> {
    let mut m = vec![0u8; MESSAGE_MAX];
    let n = encode_query(NAME, id, &mut m).unwrap();
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

    g.bench_function("encode_query", |b| {
        let mut out = [0u8; MESSAGE_MAX];
        b.iter(|| encode_query(black_box(NAME), black_box(0x1234), &mut out).unwrap())
    });

    for addrs in [1usize, 8] {
        let msg = response(0x1234, addrs);
        g.throughput(Throughput::Bytes(msg.len() as u64));
        g.bench_function(format!("parse_response/{addrs}_addrs"), |b| {
            b.iter(|| parse_response(black_box(&msg), 0x1234, black_box(NAME)).unwrap())
        });
    }

    // The rejection path matters too: stale and spoofed datagrams take it.
    let msg = response(0x1234, 8);
    g.throughput(Throughput::Bytes(msg.len() as u64));
    g.bench_function("parse_response/id_mismatch", |b| {
        b.iter(|| parse_response(black_box(&msg), 0x4321, black_box(NAME)).unwrap_err())
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
