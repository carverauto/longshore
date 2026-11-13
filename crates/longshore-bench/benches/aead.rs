// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! AES-128-GCM candidates for the TLS 1.3 record layer.
//!
//! Each candidate does what the record layer needs: take one record's worth
//! of application data from a source buffer, encrypt it together with the
//! one-byte content type that TLS 1.3 appends, and leave ciphertext and tag
//! in a destination buffer. Libraries that can only encrypt in place are
//! charged for the copy that forces, which is the point of the comparison.
//!
//! Reference from the talk: 2.2 GB/s (Zig stdlib), 7.4 GB/s (optimized), and
//! about 12% gained by passing the suffix separately instead of copying.
//!
//! All candidates must produce byte-identical records before any is timed.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;

const RECORD: usize = 16384; // largest TLS 1.3 plaintext fragment
const TAG: usize = 16;
const KEY: [u8; 16] = *b"0123456789abcdef";
const AAD: [u8; 5] = [0x17, 0x03, 0x03, 0x40, 0x11]; // a TLS 1.3 record header
const CONTENT_TYPE: [u8; 1] = [0x17];

fn nonce(seq: u64) -> [u8; 12] {
    let mut n = [0x42u8; 12];
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

/// One record: `src` (RECORD bytes) -> `dst` (RECORD + 1 + TAG bytes).
trait Sealer {
    fn seal(&self, seq: u64, src: &[u8], dst: &mut [u8]);
}

struct RingCopy(ring::aead::LessSafeKey);
impl Sealer for RingCopy {
    fn seal(&self, seq: u64, src: &[u8], dst: &mut [u8]) {
        use ring::aead::{Aad, Nonce};
        let (body, tag) = dst.split_at_mut(RECORD + 1);
        body[..RECORD].copy_from_slice(src);
        body[RECORD] = CONTENT_TYPE[0];
        let t = self
            .0
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce(seq)),
                Aad::from(AAD),
                body,
            )
            .unwrap();
        tag.copy_from_slice(t.as_ref());
    }
}

struct AwsLcCopy(aws_lc_rs::aead::LessSafeKey);
impl Sealer for AwsLcCopy {
    fn seal(&self, seq: u64, src: &[u8], dst: &mut [u8]) {
        use aws_lc_rs::aead::{Aad, Nonce};
        let (body, tag) = dst.split_at_mut(RECORD + 1);
        body[..RECORD].copy_from_slice(src);
        body[RECORD] = CONTENT_TYPE[0];
        let t = self
            .0
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce(seq)),
                Aad::from(AAD),
                body,
            )
            .unwrap();
        tag.copy_from_slice(t.as_ref());
    }
}

struct RustCryptoCopy(aes_gcm::Aes128Gcm);
impl Sealer for RustCryptoCopy {
    // The in-place trait is deprecated in favour of an in/out one; in-place is
    // what this candidate is here to measure.
    #[allow(deprecated)]
    fn seal(&self, seq: u64, src: &[u8], dst: &mut [u8]) {
        use aes_gcm::aead::AeadInPlace;
        let (body, tag) = dst.split_at_mut(RECORD + 1);
        body[..RECORD].copy_from_slice(src);
        body[RECORD] = CONTENT_TYPE[0];
        let t = self
            .0
            .encrypt_in_place_detached(&nonce(seq).into(), &AAD, body)
            .unwrap();
        tag.copy_from_slice(&t);
    }
}

/// AWS-LC's scatter interface: reads `src`, writes `dst`, and takes the
/// suffix as a separate input. No copy of the application data.
struct AwsLcScatter(*mut aws_lc_sys::EVP_AEAD_CTX);
impl Sealer for AwsLcScatter {
    fn seal(&self, seq: u64, src: &[u8], dst: &mut [u8]) {
        let n = nonce(seq);
        let (body, tail) = dst.split_at_mut(RECORD);
        let mut tail_len = 0usize;
        // The encrypted suffix byte and the tag both land in `tail`.
        let ok = unsafe {
            aws_lc_sys::EVP_AEAD_CTX_seal_scatter(
                self.0,
                body.as_mut_ptr(),
                tail.as_mut_ptr(),
                &mut tail_len,
                tail.len(),
                n.as_ptr(),
                n.len(),
                src.as_ptr(),
                src.len(),
                CONTENT_TYPE.as_ptr(),
                1,
                AAD.as_ptr(),
                AAD.len(),
            )
        };
        assert!(ok == 1 && tail_len == 1 + TAG);
    }
}

fn candidates() -> Vec<(&'static str, Box<dyn Sealer>)> {
    let scatter = unsafe {
        aws_lc_sys::EVP_AEAD_CTX_new(
            aws_lc_sys::EVP_aead_aes_128_gcm(),
            KEY.as_ptr(),
            KEY.len(),
            TAG,
        )
    };
    assert!(!scatter.is_null());
    vec![
        (
            "aes-gcm (RustCrypto), copy + in-place",
            Box::new(RustCryptoCopy(
                <aes_gcm::Aes128Gcm as aes_gcm::KeyInit>::new(&KEY.into()),
            )),
        ),
        (
            "ring, copy + in-place",
            Box::new(RingCopy(ring::aead::LessSafeKey::new(
                ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &KEY).unwrap(),
            ))),
        ),
        (
            "aws-lc-rs, copy + in-place",
            Box::new(AwsLcCopy(aws_lc_rs::aead::LessSafeKey::new(
                aws_lc_rs::aead::UnboundKey::new(&aws_lc_rs::aead::AES_128_GCM, &KEY).unwrap(),
            ))),
        ),
        ("aws-lc scatter, no copy", Box::new(AwsLcScatter(scatter))),
    ]
}

fn bench(c: &mut Criterion) {
    const RECORDS: usize = 512; // 8 MiB of application data
    let src: Vec<u8> = (0..RECORD * RECORDS)
        .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1])
        .collect();
    let all = candidates();

    // Cross-check: every candidate must emit the same bytes for the same inputs.
    let outputs: Vec<Vec<u8>> = all
        .iter()
        .map(|(_, s)| {
            let mut dst = vec![0u8; RECORD + 1 + TAG];
            s.seal(7, &src[..RECORD], &mut dst);
            dst
        })
        .collect();
    for (i, out) in outputs.iter().enumerate() {
        assert!(
            out == &outputs[0],
            "{} disagrees with {}",
            all[i].0,
            all[0].0
        );
    }
    assert_ne!(
        &outputs[0][..RECORD],
        &src[..RECORD],
        "output must be encrypted"
    );

    let mut g = c.benchmark_group("aes128gcm_tls13_record_seal/8MiB");
    g.throughput(Throughput::Bytes((RECORD * RECORDS) as u64));
    for (name, sealer) in &all {
        // One destination record reused, as a registered send buffer would be.
        let mut dst = vec![0u8; RECORD + 1 + TAG];
        g.bench_function(*name, |b| {
            b.iter(|| {
                for (seq, rec) in src.chunks_exact(RECORD).enumerate() {
                    sealer.seal(seq as u64, black_box(rec), &mut dst);
                }
                black_box(&dst);
            })
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
