// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! x86-64 kernels: `VPCLMULQDQ` on 512-bit and on 256-bit registers, and
//! `PCLMULQDQ` on 128-bit ones.

use crate::fold::{self, Bulk, Clmul, Lanes};
use crate::kernels::Kernel;
use crate::keys::Keys;
use core::arch::x86_64::*;
use std::arch::is_x86_feature_detected;

/// `PCLMULQDQ` on 128-bit registers.
struct Sse;

// SAFETY (all methods): inlined only into the functions below, which enable
// the instructions used; pointers are the caller's to keep in bounds.
impl Clmul for Sse {
    type V = __m128i;

    #[inline(always)]
    unsafe fn load(p: *const u8) -> __m128i {
        unsafe { _mm_loadu_si128(p.cast()) }
    }
    #[inline(always)]
    unsafe fn set(lo: u64, hi: u64) -> __m128i {
        unsafe { _mm_set_epi64x(hi as i64, lo as i64) }
    }
    #[inline(always)]
    unsafe fn get(v: __m128i) -> (u64, u64) {
        unsafe {
            (
                _mm_cvtsi128_si64(v) as u64,
                _mm_extract_epi64::<1>(v) as u64,
            )
        }
    }
    #[inline(always)]
    unsafe fn xor(a: __m128i, b: __m128i) -> __m128i {
        unsafe { _mm_xor_si128(a, b) }
    }
    #[inline(always)]
    unsafe fn fold(v: __m128i, k: __m128i, add: __m128i) -> __m128i {
        unsafe {
            let lo = _mm_clmulepi64_si128::<0x00>(v, k);
            let hi = _mm_clmulepi64_si128::<0x11>(v, k);
            _mm_xor_si128(_mm_xor_si128(lo, hi), add)
        }
    }
    #[inline(always)]
    unsafe fn mul(a: u64, b: u64) -> (u64, u64) {
        unsafe {
            let (a, b) = (_mm_cvtsi64_si128(a as i64), _mm_cvtsi64_si128(b as i64));
            Self::get(_mm_clmulepi64_si128::<0x00>(a, b))
        }
    }
    #[inline(always)]
    unsafe fn crc32c_u64(state: u32, v: u64) -> u32 {
        unsafe { _mm_crc32_u64(state as u64, v) as u32 }
    }
    #[inline(always)]
    unsafe fn crc32c_u8(state: u32, v: u8) -> u32 {
        unsafe { _mm_crc32_u8(state, v) }
    }
}

/// `M` registers of 256 bits, two blocks each.
struct Ymm<const M: usize>;

impl<const M: usize> Bulk for Ymm<M> {
    type C = Sse;

    #[inline(always)]
    unsafe fn bulk<'a>(acc: __m128i, rest: &'a [u8], keys: &Keys) -> (__m128i, &'a [u8]) {
        const W: usize = 32;
        if rest.len() < 2 * M * W {
            return (acc, rest);
        }
        // SAFETY: every read is below `rest.len()`: the first `M` registers
        // by the check above, later ones by the loop condition.
        unsafe {
            let p = rest.as_ptr();
            let key = |j: usize| _mm256_broadcastsi128_si256(Sse::set(keys[j][0], keys[j][1]));
            let (k1, kw, km) = (Sse::set(keys[1][0], keys[1][1]), key(2), key(2 * M));
            let fold = |v: __m256i, k: __m256i, add: __m256i| {
                let lo = _mm256_clmulepi64_epi128::<0x00>(v, k);
                let hi = _mm256_clmulepi64_epi128::<0x11>(v, k);
                _mm256_xor_si256(_mm256_xor_si256(lo, hi), add)
            };
            let mut v = [_mm256_setzero_si256(); M];
            for (i, r) in v.iter_mut().enumerate() {
                *r = _mm256_loadu_si256(p.add(i * W).cast());
            }
            // One block further along, `acc` lines up with the first block.
            let first = Sse::fold(acc, k1, _mm256_castsi256_si128(v[0]));
            v[0] = _mm256_inserti128_si256::<0>(v[0], first);
            let mut at = M * W;
            while rest.len() - at >= M * W {
                for (i, r) in v.iter_mut().enumerate() {
                    *r = fold(*r, km, _mm256_loadu_si256(p.add(at + i * W).cast()));
                }
                at += M * W;
            }
            let mut x = v[0];
            for r in &v[1..] {
                x = fold(x, kw, *r);
            }
            let acc = _mm256_castsi256_si128(x);
            let acc = Sse::fold(acc, k1, _mm256_extracti128_si256::<1>(x));
            (acc, &rest[at..])
        }
    }
}

/// `M` registers of 512 bits, four blocks each.
struct Zmm<const M: usize>;

impl<const M: usize> Bulk for Zmm<M> {
    type C = Sse;

    #[inline(always)]
    unsafe fn bulk<'a>(acc: __m128i, rest: &'a [u8], keys: &Keys) -> (__m128i, &'a [u8]) {
        const W: usize = 64;
        if rest.len() < 2 * M * W {
            return (acc, rest);
        }
        // SAFETY: as for `Ymm`.
        unsafe {
            let p = rest.as_ptr();
            let key = |j: usize| _mm512_broadcast_i32x4(Sse::set(keys[j][0], keys[j][1]));
            let (k1, kw, km) = (Sse::set(keys[1][0], keys[1][1]), key(4), key(4 * M));
            let fold = |v: __m512i, k: __m512i, add: __m512i| {
                let lo = _mm512_clmulepi64_epi128::<0x00>(v, k);
                let hi = _mm512_clmulepi64_epi128::<0x11>(v, k);
                // 0x96 is the three-way exclusive or.
                _mm512_ternarylogic_epi64::<0x96>(lo, hi, add)
            };
            let mut v = [_mm512_setzero_si512(); M];
            for (i, r) in v.iter_mut().enumerate() {
                *r = _mm512_loadu_si512(p.add(i * W).cast());
            }
            let first = Sse::fold(acc, k1, _mm512_castsi512_si128(v[0]));
            v[0] = _mm512_inserti32x4::<0>(v[0], first);
            let mut at = M * W;
            while rest.len() - at >= M * W {
                for (i, r) in v.iter_mut().enumerate() {
                    *r = fold(*r, km, _mm512_loadu_si512(p.add(at + i * W).cast()));
                }
                at += M * W;
            }
            let mut x = v[0];
            for r in &v[1..] {
                x = fold(x, kw, *r);
            }
            let acc = _mm512_castsi512_si128(x);
            let acc = Sse::fold(acc, k1, _mm512_extracti32x4_epi32::<1>(x));
            let acc = Sse::fold(acc, k1, _mm512_extracti32x4_epi32::<2>(x));
            let acc = Sse::fold(acc, k1, _mm512_extracti32x4_epi32::<3>(x));
            (acc, &rest[at..])
        }
    }
}

macro_rules! kernel {
    ($crc64:ident, $crc32c:ident, $bulk:ty, $features:literal) => {
        #[target_feature(enable = $features)]
        unsafe fn $crc64(state: u64, data: &[u8]) -> u64 {
            unsafe { fold::crc64::<$bulk>(state, data) }
        }
        #[target_feature(enable = $features)]
        unsafe fn $crc32c(state: u32, data: &[u8]) -> u32 {
            unsafe { fold::crc32c::<$bulk>(state, data) }
        }
    };
}
kernel!(
    zmm4_crc64,
    zmm4_crc32c,
    Zmm<4>,
    "avx512f,avx512vl,vpclmulqdq,avx2,pclmulqdq,sse4.2"
);
kernel!(
    zmm8_crc64,
    zmm8_crc32c,
    Zmm<8>,
    "avx512f,avx512vl,vpclmulqdq,avx2,pclmulqdq,sse4.2"
);
kernel!(
    ymm4_crc64,
    ymm4_crc32c,
    Ymm<4>,
    "avx2,vpclmulqdq,pclmulqdq,sse4.2"
);
kernel!(
    ymm8_crc64,
    ymm8_crc32c,
    Ymm<8>,
    "avx2,vpclmulqdq,pclmulqdq,sse4.2"
);
kernel!(sse8_crc64, sse8_crc32c, Lanes<Sse, 8>, "pclmulqdq,sse4.2");
kernel!(sse4_crc64, sse4_crc32c, Lanes<Sse, 4>, "pclmulqdq,sse4.2");

macro_rules! fused {
    ($crc32c:ident, $lanes:literal, $words:literal) => {
        #[target_feature(enable = "pclmulqdq,sse4.2")]
        unsafe fn $crc32c(state: u32, data: &[u8]) -> u32 {
            unsafe { fold::crc32c_fused::<Sse, $lanes, $words>(state, data) }
        }
    };
}
fused!(sse8_fused2, 8, 2);
fused!(sse4_fused3, 4, 3);

fn has_128() -> bool {
    is_x86_feature_detected!("pclmulqdq") && is_x86_feature_detected!("sse4.2")
}
fn has_256() -> bool {
    has_128() && is_x86_feature_detected!("avx2") && is_x86_feature_detected!("vpclmulqdq")
}
fn has_512() -> bool {
    has_256() && is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512vl")
}

/// In order of preference within each width; the table kernel is last. A
/// name gives the registers folded side by side, then for CRC-32C the words
/// each of three CRC instruction streams reads beside them ("+3xS"), if any.
pub(crate) static KERNELS: [Kernel; 9] = [
    Kernel::new("avx512-vpclmulqdq x4", has_512, zmm4_crc64, zmm4_crc32c),
    Kernel::new("avx512-vpclmulqdq x8", has_512, zmm8_crc64, zmm8_crc32c),
    Kernel::new("avx2-vpclmulqdq x4", has_256, ymm4_crc64, ymm4_crc32c),
    Kernel::new("avx2-vpclmulqdq x8", has_256, ymm8_crc64, ymm8_crc32c),
    Kernel::new("sse-pclmulqdq x8", has_128, sse8_crc64, sse8_crc32c),
    Kernel::new("sse-pclmulqdq x4", has_128, sse4_crc64, sse4_crc32c),
    Kernel::new("sse-pclmulqdq x8 +3x2", has_128, sse8_crc64, sse8_fused2),
    Kernel::new("sse-pclmulqdq x4 +3x3", has_128, sse4_crc64, sse4_fused3),
    Kernel::TABLE,
];

/// The index in `KERNELS` of the kernel to use on this processor.
#[inline]
pub(crate) fn pick() -> usize {
    if has_512() {
        0
    } else if has_256() {
        2
    } else if has_128() {
        4
    } else {
        KERNELS.len() - 1
    }
}
