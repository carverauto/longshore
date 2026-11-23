// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! AES-GCM record sealing with the memory pattern of an HTTPS upload.
//!
//! `aead.rs` seals every record into one 16 KiB destination that is reused,
//! so its figure is the cipher with its output in the processor's cache. The
//! client does not work like that: each connection seals megabytes into its
//! own send buffer, which has to stay as it is until the network card has
//! taken it, and the bodies it reads are different memory each time. This
//! measures the same AWS-LC call in four patterns, from the first to the
//! client's:
//!
//! - `hot`: one 8 MiB source, one 16 KiB destination. What `aead.rs` measures.
//! - `batch`: one 8 MiB source, sealed into one 8 MiB destination.
//! - `cold`: many sources and many destinations, taken in turn, so both are
//!   out of the cache when they are touched.
//! - `cold, in place`: many sources, each sealed over itself, with only the
//!   17 bytes that follow a record written elsewhere.
//!
//! The cold cases use `SETS` (32) bodies of 8 MiB, 256 MiB each of source
//! and destination.

use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, Throughput};
use std::hint::black_box;

const RECORD: usize = 16384; // largest TLS 1.3 plaintext fragment
const TAG: usize = 16;
const TAIL: usize = 1 + TAG; // the sealed content type, then the tag
const SLOT: usize = RECORD + TAIL; // a record's space in a destination, less its header
const RECORDS: usize = 512; // 8 MiB of application data
const BODY: usize = RECORD * RECORDS;
const SETS: usize = 32; // bodies in the cold working set, 256 MiB of each
const KEY: [u8; 16] = *b"0123456789abcdef";
const AAD: [u8; 5] = [0x17, 0x03, 0x03, 0x40, 0x11]; // a TLS 1.3 record header
const CONTENT_TYPE: [u8; 1] = [0x17];

fn nonce(seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

struct Cipher(*mut aws_lc_sys::EVP_AEAD_CTX);

impl Cipher {
    fn new() -> Cipher {
        let ctx = unsafe {
            aws_lc_sys::EVP_AEAD_CTX_new(
                aws_lc_sys::EVP_aead_aes_128_gcm(),
                KEY.as_ptr(),
                KEY.len(),
                TAG,
            )
        };
        assert!(!ctx.is_null());
        Cipher(ctx)
    }

    /// Seal `len` bytes at `src` to `out`, which is either `src` itself or
    /// memory that does not overlap it, and the 17 bytes after them to `tail`.
    ///
    /// # Safety
    /// `src` and `out` are valid for `len` bytes and `tail` for `TAIL`.
    unsafe fn seal(&self, seq: u64, src: *const u8, len: usize, out: *mut u8, tail: *mut u8) {
        let n = nonce(seq);
        let mut tail_len = 0usize;
        let ok = unsafe {
            aws_lc_sys::EVP_AEAD_CTX_seal_scatter(
                self.0,
                out,
                tail,
                &mut tail_len,
                TAIL,
                n.as_ptr(),
                n.len(),
                src,
                len,
                CONTENT_TYPE.as_ptr(),
                1,
                AAD.as_ptr(),
                AAD.len(),
            )
        };
        assert!(ok == 1 && tail_len == TAIL);
    }

    /// One body, each record sealed into its own slot of `dst`.
    fn body_to(&self, src: &[u8], dst: &mut [u8]) {
        assert!(src.len() == BODY && dst.len() == SLOT * RECORDS);
        for (seq, (rec, slot)) in src
            .as_chunks::<RECORD>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<SLOT>().0.iter_mut())
            .enumerate()
        {
            let (body, tail) = slot.split_at_mut(RECORD);
            // SAFETY: the slices have the lengths the call is given.
            unsafe {
                self.seal(
                    seq as u64,
                    rec.as_ptr(),
                    RECORD,
                    body.as_mut_ptr(),
                    tail.as_mut_ptr(),
                )
            };
        }
    }

    /// One body, each record sealed over itself. `tails` takes what follows.
    fn body_in_place(&self, body: &mut [u8], tails: &mut [u8]) {
        assert!(body.len() == BODY && tails.len() == TAIL * RECORDS);
        for (seq, (rec, tail)) in body
            .as_chunks_mut::<RECORD>()
            .0
            .iter_mut()
            .zip(tails.as_chunks_mut::<TAIL>().0.iter_mut())
            .enumerate()
        {
            let p = rec.as_mut_ptr();
            // SAFETY: input and output are the same `RECORD` bytes, which
            // the scatter call allows; the tail is separate memory.
            unsafe { self.seal(seq as u64, p, RECORD, p, tail.as_mut_ptr()) };
        }
    }
}

fn filled(len: usize, pattern: u32) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (i as u32)
                .wrapping_add(pattern)
                .wrapping_mul(2654435761)
                .to_le_bytes()[1]
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let cipher = Cipher::new();

    // In place and out of place must agree, or the comparison means nothing.
    let src = filled(BODY, 1);
    let mut apart = vec![0u8; SLOT * RECORDS];
    cipher.body_to(&src, &mut apart);
    let (mut over, mut tails) = (src.clone(), vec![0u8; TAIL * RECORDS]);
    cipher.body_in_place(&mut over, &mut tails);
    for r in [0, 1, RECORDS - 1] {
        assert_eq!(over[r * RECORD..][..RECORD], apart[r * SLOT..][..RECORD]);
        assert_eq!(
            tails[r * TAIL..][..TAIL],
            apart[r * SLOT + RECORD..][..TAIL]
        );
    }
    assert_ne!(over[..RECORD], src[..RECORD], "output must be encrypted");

    let mut g = c.benchmark_group("aes128gcm_tls13_record_seal_memory/8MiB");
    let _: &mut BenchmarkGroup<'_, WallTime> = g.throughput(Throughput::Bytes(BODY as u64));

    let mut one = vec![0u8; SLOT];
    let _: &mut BenchmarkGroup<'_, WallTime> =
        g.bench_function("hot: 16 KiB destination reused", |b| {
            b.iter(|| {
                for (seq, rec) in src.as_chunks::<RECORD>().0.iter().enumerate() {
                    let (body, tail) = one.split_at_mut(RECORD);
                    // SAFETY: the slices have the lengths the call is given.
                    unsafe {
                        cipher.seal(
                            seq as u64,
                            black_box(rec).as_ptr(),
                            RECORD,
                            body.as_mut_ptr(),
                            tail.as_mut_ptr(),
                        )
                    };
                }
                let _: &Vec<u8> = black_box(&one);
            })
        });

    let mut dst = vec![0u8; SLOT * RECORDS];
    let _: &mut BenchmarkGroup<'_, WallTime> =
        g.bench_function("batch: 8 MiB destination reused", |b| {
            b.iter(|| {
                cipher.body_to(black_box(&src), &mut dst);
                let _: &Vec<u8> = black_box(&dst);
            })
        });

    let sources: Vec<Vec<u8>> = (0..SETS).map(|i| filled(BODY, i as u32)).collect();
    let mut dests: Vec<Vec<u8>> = (0..SETS).map(|_| vec![1u8; SLOT * RECORDS]).collect();
    let mut at = 0usize;
    let _: &mut BenchmarkGroup<'_, WallTime> =
        g.bench_function(format!("cold: {SETS} sources, {SETS} destinations"), |b| {
            b.iter(|| {
                at = (at + 1) % SETS;
                cipher.body_to(black_box(&sources[at]), &mut dests[at]);
                let _: &Vec<u8> = black_box(&dests[at]);
            })
        });
    drop(dests);

    let mut bodies = sources;
    let mut at = 0usize;
    let _: &mut BenchmarkGroup<'_, WallTime> =
        g.bench_function(format!("cold, in place: {SETS} bodies"), |b| {
            b.iter(|| {
                at = (at + 1) % SETS;
                cipher.body_in_place(black_box(&mut bodies[at]), &mut tails);
                let _: &Vec<u8> = black_box(&bodies[at]);
            })
        });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
