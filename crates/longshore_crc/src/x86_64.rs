// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! x86-64 kernels: `VPCLMULQDQ` on 512-bit and on 256-bit registers, and
//! `PCLMULQDQ` on 128-bit ones.

use crate::fold::{self, Clmul, Lanes, One, Reg, Wide};
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
        unsafe { _mm_set_epi64x(hi.cast_signed(), lo.cast_signed()) }
    }
    #[inline(always)]
    unsafe fn get(v: __m128i) -> (u64, u64) {
        unsafe {
            (
                _mm_cvtsi128_si64(v).cast_unsigned(),
                _mm_extract_epi64::<1>(v).cast_unsigned(),
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
            let (a, b) = (
                _mm_cvtsi64_si128(a.cast_signed()),
                _mm_cvtsi64_si128(b.cast_signed()),
            );
            Self::get(_mm_clmulepi64_si128::<0x00>(a, b))
        }
    }
    #[inline(always)]
    unsafe fn crc32c_u64(state: u32, v: u64) -> u32 {
        unsafe { _mm_crc32_u64(u64::from(state), v) as u32 }
    }
    #[inline(always)]
    unsafe fn crc32c_u8(state: u32, v: u8) -> u32 {
        unsafe { _mm_crc32_u8(state, v) }
    }
}

/// A 256-bit register: two blocks.
struct Ymm;

// SAFETY (all methods): as for `Sse`.
impl Reg for Ymm {
    type C = Sse;
    type R = __m256i;
    const BLOCKS: usize = 2;

    #[inline(always)]
    unsafe fn load(p: *const u8) -> __m256i {
        unsafe { _mm256_loadu_si256(p.cast()) }
    }
    #[inline(always)]
    unsafe fn key(keys: &Keys, blocks: usize) -> __m256i {
        unsafe { _mm256_broadcastsi128_si256(Sse::set(keys[blocks][0], keys[blocks][1])) }
    }
    #[inline(always)]
    unsafe fn fold(v: __m256i, k: __m256i, add: __m256i) -> __m256i {
        unsafe {
            let lo = _mm256_clmulepi64_epi128::<0x00>(v, k);
            let hi = _mm256_clmulepi64_epi128::<0x11>(v, k);
            _mm256_xor_si256(_mm256_xor_si256(lo, hi), add)
        }
    }
    #[inline(always)]
    unsafe fn enter(acc: __m128i, v: __m256i, keys: &Keys) -> __m256i {
        unsafe {
            let k1 = Sse::set(keys[1][0], keys[1][1]);
            let first = Sse::fold(acc, k1, _mm256_castsi256_si128(v));
            _mm256_inserti128_si256::<0>(v, first)
        }
    }
    #[inline(always)]
    unsafe fn narrow(v: __m256i, keys: &Keys) -> __m128i {
        unsafe {
            let k1 = Sse::set(keys[1][0], keys[1][1]);
            let last = _mm256_extracti128_si256::<1>(v);
            Sse::fold(_mm256_castsi256_si128(v), k1, last)
        }
    }
}

/// A 512-bit register: four blocks.
struct Zmm;

// SAFETY (all methods): as for `Sse`.
impl Reg for Zmm {
    type C = Sse;
    type R = __m512i;
    const BLOCKS: usize = 4;

    #[inline(always)]
    unsafe fn load(p: *const u8) -> __m512i {
        unsafe { _mm512_loadu_si512(p.cast()) }
    }
    #[inline(always)]
    unsafe fn key(keys: &Keys, blocks: usize) -> __m512i {
        unsafe { _mm512_broadcast_i32x4(Sse::set(keys[blocks][0], keys[blocks][1])) }
    }
    #[inline(always)]
    unsafe fn fold(v: __m512i, k: __m512i, add: __m512i) -> __m512i {
        unsafe {
            let lo = _mm512_clmulepi64_epi128::<0x00>(v, k);
            let hi = _mm512_clmulepi64_epi128::<0x11>(v, k);
            // 0x96 is the three-way exclusive or.
            _mm512_ternarylogic_epi64::<0x96>(lo, hi, add)
        }
    }
    #[inline(always)]
    unsafe fn enter(acc: __m128i, v: __m512i, keys: &Keys) -> __m512i {
        unsafe {
            let k1 = Sse::set(keys[1][0], keys[1][1]);
            let first = Sse::fold(acc, k1, _mm512_castsi512_si128(v));
            _mm512_inserti32x4::<0>(v, first)
        }
    }
    /// Each block but the last moves to the last one's place by a constant
    /// of its own, so the three multiplications run side by side.
    #[inline(always)]
    unsafe fn narrow(v: __m512i, keys: &Keys) -> __m128i {
        unsafe {
            let key = |j: usize| Sse::set(keys[j][0], keys[j][1]);
            let acc = _mm512_extracti32x4_epi32::<3>(v);
            let acc = Sse::fold(_mm512_castsi512_si128(v), key(3), acc);
            let acc = Sse::fold(_mm512_extracti32x4_epi32::<1>(v), key(2), acc);
            Sse::fold(_mm512_extracti32x4_epi32::<2>(v), key(1), acc)
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
kernel!(zmm4_crc64, zmm4_crc32c, Wide<Zmm, 4>, "avx512f,avx512vl,vpclmulqdq,avx2,pclmulqdq,sse4.2");
kernel!(zmm8_crc64, zmm8_crc32c, Wide<Zmm, 8>, "avx512f,avx512vl,vpclmulqdq,avx2,pclmulqdq,sse4.2");
kernel!(ymm4_crc64, ymm4_crc32c, Wide<Ymm, 4>, "avx2,vpclmulqdq,pclmulqdq,sse4.2");
kernel!(ymm8_crc64, ymm8_crc32c, Wide<Ymm, 8>, "avx2,vpclmulqdq,pclmulqdq,sse4.2");
kernel!(sse8_crc64, sse8_crc32c, Lanes<Sse, 8>, "pclmulqdq,sse4.2");
kernel!(sse4_crc64, sse4_crc32c, Lanes<Sse, 4>, "pclmulqdq,sse4.2");

/// CRC-32C with three CRC instruction streams beside `$m` registers of
/// kind `$reg`, each stream reading `$words` words a round.
macro_rules! fused {
    ($crc32c:ident, $reg:ty, $m:literal, $words:literal, $features:literal) => {
        #[target_feature(enable = $features)]
        unsafe fn $crc32c(state: u32, data: &[u8]) -> u32 {
            unsafe { fold::crc32c_fused::<$reg, $m, $words>(state, data) }
        }
    };
}
fused!(
    zmm4_fused3,
    Zmm,
    4,
    3,
    "avx512f,avx512vl,vpclmulqdq,avx2,pclmulqdq,sse4.2"
);
fused!(
    zmm4_fused6,
    Zmm,
    4,
    6,
    "avx512f,avx512vl,vpclmulqdq,avx2,pclmulqdq,sse4.2"
);
fused!(ymm8_fused3, Ymm, 8, 3, "avx2,vpclmulqdq,pclmulqdq,sse4.2");
fused!(ymm4_fused2, Ymm, 4, 2, "avx2,vpclmulqdq,pclmulqdq,sse4.2");
fused!(sse8_fused2, One<Sse>, 8, 2, "pclmulqdq,sse4.2");
fused!(sse4_fused3, One<Sse>, 4, 3, "pclmulqdq,sse4.2");

/// CRC-32C for processors with the 128-bit multiply only. Their CRC
/// instruction is strong beside it, so long inputs go to four lanes with
/// three CRC streams: measured on Cascade Lake at 64 KiB, 42.8 GiB/s
/// against 26.2 for eight lanes alone. Short inputs keep eight lanes.
#[target_feature(enable = "pclmulqdq,sse4.2")]
unsafe fn sse_crc32c(state: u32, data: &[u8]) -> u32 {
    unsafe {
        if data.len() < fold::fused_min(4 * 16, 3) {
            fold::crc32c::<Lanes<Sse, 8>>(state, data)
        } else {
            fold::crc32c_fused::<One<Sse>, 4, 3>(state, data)
        }
    }
}

fn has_128() -> bool {
    is_x86_feature_detected!("pclmulqdq") && is_x86_feature_detected!("sse4.2")
}
fn has_256() -> bool {
    has_128() && is_x86_feature_detected!("avx2") && is_x86_feature_detected!("vpclmulqdq")
}
fn has_512() -> bool {
    has_256() && is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512vl")
}

/// The table kernel is last. A name gives the registers folded side by
/// side, then for CRC-32C the words each of three CRC instruction streams
/// reads beside them ("+3xS"), if any. `PICK_*` name the ones selected.
pub(crate) static KERNELS: [Kernel; 14] = [
    Kernel::new(
        "avx512-vpclmulqdq x4 +3x3",
        has_512,
        zmm4_crc64,
        zmm4_fused3,
    ),
    Kernel::new(
        "avx512-vpclmulqdq x4 +3x6",
        has_512,
        zmm4_crc64,
        zmm4_fused6,
    ),
    Kernel::new("avx512-vpclmulqdq x4", has_512, zmm4_crc64, zmm4_crc32c),
    Kernel::new("avx512-vpclmulqdq x8", has_512, zmm8_crc64, zmm8_crc32c),
    Kernel::new("avx2-vpclmulqdq x4", has_256, ymm4_crc64, ymm4_crc32c),
    Kernel::new("avx2-vpclmulqdq x8", has_256, ymm8_crc64, ymm8_crc32c),
    Kernel::new("avx2-vpclmulqdq x8 +3x3", has_256, ymm8_crc64, ymm8_fused3),
    Kernel::new("avx2-vpclmulqdq x4 +3x2", has_256, ymm4_crc64, ymm4_fused2),
    Kernel::new(
        "sse-pclmulqdq x8, long +3x3 on x4",
        has_128,
        sse8_crc64,
        sse_crc32c,
    ),
    Kernel::new("sse-pclmulqdq x8", has_128, sse8_crc64, sse8_crc32c),
    Kernel::new("sse-pclmulqdq x4", has_128, sse4_crc64, sse4_crc32c),
    Kernel::new("sse-pclmulqdq x8 +3x2", has_128, sse8_crc64, sse8_fused2),
    Kernel::new("sse-pclmulqdq x4 +3x3", has_128, sse4_crc64, sse4_fused3),
    Kernel::TABLE,
];
const PICK_512: usize = 0;
const PICK_256: usize = 4;
const PICK_128: usize = 8;

/// The index in `KERNELS` of the kernel to use on this processor.
#[inline]
pub(crate) fn pick() -> usize {
    if has_512() {
        PICK_512
    } else if has_256() {
        PICK_256
    } else if has_128() {
        PICK_128
    } else {
        KERNELS.len() - 1
    }
}
