// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! longshore's TLS record layer with each provider: 512 records of 16 KiB
//! (8 MiB of application data) sealed, and sealed then opened, with
//! AES-128-GCM keys from a real handshake (rustls's keys can only come from
//! one). The send buffer and the destination are reused, as registered
//! buffers would be.

use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, Throughput};
use longshore::tls::handshake::{ClientSetup, Handshake, ServerSetup};
use longshore::tls::provider::Provider;
use longshore::tls::record::{Direction, CONTENT_APPLICATION_DATA, FRAGMENT_MAX, RECORD_MAX};
use std::hint::black_box;

const NAME: &str = "bench.test";
const RECORDS: usize = 512;

fn failed(what: &str) -> ! {
    eprintln!("tls_providers: {what}");
    std::process::exit(1)
}

/// A client's sending direction and the server's receiving one, keyed by a
/// handshake between a client and a server that both use `provider`.
fn pair(provider: Provider) -> (Direction, Direction) {
    let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()])
        .unwrap_or_else(|_| failed("certificate"));
    let (cert, key) = (k.cert.der().to_vec(), k.signing_key.serialize_der());
    let cs = ClientSetup::with_roots_using(provider, &[&cert]).unwrap_or_else(|_| failed("client"));
    let ss = ServerSetup::new_using(provider, &[&cert], &key).unwrap_or_else(|_| failed("server"));
    let mut c = Handshake::client(&cs, NAME).unwrap_or_else(|_| failed("client handshake"));
    let mut s = Handshake::server(&ss).unwrap_or_else(|_| failed("server handshake"));
    let (mut a, mut b) = (vec![0u8; 32_768], vec![0u8; 32_768]);
    let (mut an, mut bn) = (0usize, 0usize);
    while !(c.is_done() && s.is_done() && an == 0) {
        an += c.write(&mut a[an..]);
        bn += s.write(&mut b[bn..]);
        let s_used = s.read(&a[..an]).unwrap_or_else(|_| failed("server read"));
        a.copy_within(s_used..an, 0);
        an -= s_used;
        let c_used = c.read(&b[..bn]).unwrap_or_else(|_| failed("client read"));
        b.copy_within(c_used..bn, 0);
        bn -= c_used;
    }
    let (mut ctx, mut crx, mut stx, mut srx) = (
        Direction::new(),
        Direction::new(),
        Direction::new(),
        Direction::new(),
    );
    c.finish(&mut ctx, &mut crx)
        .unwrap_or_else(|_| failed("client finish"));
    s.finish(&mut stx, &mut srx)
        .unwrap_or_else(|_| failed("server finish"));
    (ctx, srx)
}

fn bench(c: &mut Criterion) {
    let src: Vec<u8> = (0..FRAGMENT_MAX * RECORDS)
        .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1])
        .collect();
    let mut g = c.benchmark_group("tls_record_aes128gcm/8MiB");
    let _: &mut BenchmarkGroup<'_, WallTime> =
        g.throughput(Throughput::Bytes((FRAGMENT_MAX * RECORDS) as u64));
    for provider in Provider::ALL.into_iter().filter(|p| p.compiled()) {
        let (mut sealer, _) = pair(provider);
        let mut rec = vec![0u8; RECORD_MAX];
        let mut out = vec![0u8; RECORD_MAX];
        let name = provider.feature();
        let _: &mut BenchmarkGroup<'_, WallTime> = g.bench_function(format!("{name} seal"), |b| {
            b.iter(|| {
                for chunk in src.chunks(FRAGMENT_MAX) {
                    let n = sealer
                        .seal(CONTENT_APPLICATION_DATA, black_box(chunk), &mut rec)
                        .unwrap_or_else(|_| failed("seal"));
                    let _: usize = black_box(n);
                }
            })
        });
        // Sealing moved `tx` ahead of `rx`; a fresh pair keeps them in step.
        let (mut tx, mut rx) = pair(provider);
        let _: &mut BenchmarkGroup<'_, WallTime> =
            g.bench_function(format!("{name} seal then open"), |b| {
                b.iter(|| {
                    for chunk in src.chunks(FRAGMENT_MAX) {
                        let n = tx
                            .seal(CONTENT_APPLICATION_DATA, black_box(chunk), &mut rec)
                            .unwrap_or_else(|_| failed("seal"));
                        let room = rx.open_room(n);
                        let got = rx
                            .open(&rec[..n], &mut out[..room])
                            .unwrap_or_else(|_| failed("open"));
                        let _: (u8, usize) = black_box(got);
                    }
                })
            });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
