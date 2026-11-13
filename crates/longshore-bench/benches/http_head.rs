// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP head microbenchmarks: what the client does per request besides I/O.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use longshore::http::head::{parse_response, HeadWriter};
use std::hint::black_box;

/// Shaped like an S3 PutObject response.
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nx-amz-id-2: LriYPLdmOdAiIfgSm/F1YsViT1LW94/xUQxMsF7xiEb1a0wiIOIxl+zbwZ163pt7\r\nx-amz-request-id: 0A49CE4060975EAC\r\nDate: Wed, 12 Oct 2009 17:50:00 GMT\r\nETag: \"1b2cf535f27731c974343645a3985328\"\r\nx-amz-checksum-crc64nvme: AAAAAAAAAAA=\r\nContent-Length: 0\r\nConnection: keep-alive\r\nServer: AmazonS3\r\n\r\n";

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("http_head");
    g.throughput(Throughput::Bytes(RESPONSE.len() as u64));
    g.bench_function("parse_response", |b| {
        b.iter(|| parse_response(black_box(RESPONSE), 4096).unwrap().unwrap())
    });
    g.bench_function("parse_response_then_lookup", |b| {
        b.iter(|| {
            let h = parse_response(black_box(RESPONSE), 4096).unwrap().unwrap();
            h.fields.header(RESPONSE, "etag").unwrap().len()
        })
    });
    g.throughput(Throughput::Elements(1));
    g.bench_function("write_put_request", |b| {
        let mut buf = [0u8; 1024];
        b.iter(|| {
            let mut w = HeadWriter::request(&mut buf, "PUT", black_box("/bucket/some/object/key-000123"));
            w.header("Host", b"bucket.us-east-1.linodeobjects.com").header_u64("Content-Length", 8_388_608).header("x-amz-content-sha256", b"UNSIGNED-PAYLOAD").header("x-amz-date", b"20261112T101500Z").header("Authorization", b"AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20261112/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=fe5f80f77d5fa3beca038a248ff027d0445342fe2855ddc963176630326f1024");
            w.finish().unwrap()
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
