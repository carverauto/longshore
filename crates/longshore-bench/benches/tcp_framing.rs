// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TCP framing microbenchmarks: what reading a frame's length from its
//! header costs, for the framings a protocol is likely to use, and for
//! headers that are refused.
//!
//!   cargo bench -p longshore-bench --bench tcp_framing
//!   cargo bench -p longshore-bench --bench tcp_framing -- --save-baseline main

use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, Throughput};
use longshore::tcp::{FrameError, FramingError, HeaderFraming};
use std::hint::black_box;

/// The longest frame every framing here accepts, header included.
const MAX_FRAME: u32 = 1 << 20;
/// The body the accepted headers announce: one Ethernet payload.
const BODY: u32 = 1500;

/// The framing, or stop naming `what`.
fn usable(framing: Result<HeaderFraming, FramingError>, what: &str) -> HeaderFraming {
    match framing {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tcp_framing: {what}: {e:?}");
            std::process::exit(1)
        }
    }
}

/// A header of `len` filler bytes with `field` written at `at`.
fn header(len: usize, at: usize, field: &[u8]) -> Vec<u8> {
    let mut h: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37)).collect();
    h[at..at + field.len()].copy_from_slice(field);
    h
}

/// Time `frame_len` of `framing` on `head`, which must give `want`.
fn case(
    g: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    framing: HeaderFraming,
    head: &[u8],
    want: Result<usize, FrameError>,
) {
    assert_eq!(framing.frame_len(head), want, "{name}");
    let _: &mut BenchmarkGroup<'_, WallTime> = g.bench_function(name, |b| {
        b.iter(|| black_box(&framing).frame_len(black_box(head)))
    });
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("tcp_framing");
    let _: &mut BenchmarkGroup<'_, WallTime> = g.throughput(Throughput::Elements(1));
    let body = BODY as usize;

    // Four bytes, big-endian, giving the length of what follows.
    let prefix = usable(HeaderFraming::length_prefix(MAX_FRAME), "length prefix");
    let short = header(4, 0, &BODY.to_be_bytes());
    case(&mut g, "length_prefix", prefix, &short, Ok(4 + body));

    // A large header with a little-endian length in the middle of it.
    let wide = usable(
        HeaderFraming::new(256, 100, 4, false, false, MAX_FRAME),
        "256-byte header",
    );
    let long = header(256, 100, &BODY.to_le_bytes());
    case(&mut g, "header_256_le", wide, &long, Ok(256 + body));

    // An eight-byte length that counts its header of sixteen bytes.
    let eight = usable(
        HeaderFraming::new(16, 8, 8, true, true, MAX_FRAME),
        "eight-byte field",
    );
    let counted = header(16, 8, &u64::from(16 + BODY).to_be_bytes());
    case(&mut g, "field_8_be", eight, &counted, Ok(16 + body));

    // The refusals matter too: a peer that sends them is cut off on this path.
    let huge = header(4, 0, &u32::MAX.to_be_bytes());
    case(
        &mut g,
        "too_large",
        prefix,
        &huge,
        Err(FrameError::TooLarge),
    );
    let tiny = header(16, 8, &3u64.to_be_bytes());
    case(&mut g, "too_small", eight, &tiny, Err(FrameError::TooSmall));

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
