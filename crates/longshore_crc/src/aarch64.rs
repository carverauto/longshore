// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! AArch64 kernels: `PMULL`, with `EOR3` where the processor has it.

use crate::fold::{self, Clmul, Lanes};
use crate::kernels::Kernel;
use core::arch::aarch64::*;
use std::arch::is_aarch64_feature_detected;

/// `PMULL` on 128-bit registers. `EOR3` adds three values in one instruction.
struct Pmull<const EOR3: bool>;

// SAFETY (all methods): inlined only into the functions below, which enable
// the instructions used; pointers are the caller's to keep in bounds.
impl<const EOR3: bool> Clmul for Pmull<EOR3> {
    type V = uint8x16_t;

    #[inline(always)]
    unsafe fn load(p: *const u8) -> uint8x16_t {
        unsafe { vld1q_u8(p) }
    }
    #[inline(always)]
    unsafe fn set(lo: u64, hi: u64) -> uint8x16_t {
        unsafe { vreinterpretq_u8_u64(vcombine_u64(vcreate_u64(lo), vcreate_u64(hi))) }
    }
    #[inline(always)]
    unsafe fn get(v: uint8x16_t) -> (u64, u64) {
        unsafe {
            let v = vreinterpretq_u64_u8(v);
            (vgetq_lane_u64::<0>(v), vgetq_lane_u64::<1>(v))
        }
    }
    #[inline(always)]
    unsafe fn xor(a: uint8x16_t, b: uint8x16_t) -> uint8x16_t {
        unsafe { veorq_u8(a, b) }
    }
    #[inline(always)]
    unsafe fn fold(v: uint8x16_t, k: uint8x16_t, add: uint8x16_t) -> uint8x16_t {
        unsafe {
            let (v, k) = (vreinterpretq_p64_u8(v), vreinterpretq_p64_u8(k));
            let lo = vmull_p64(vgetq_lane_p64::<0>(v), vgetq_lane_p64::<0>(k));
            let hi = vmull_high_p64(v, k);
            let (lo, hi) = (vreinterpretq_u8_p128(lo), vreinterpretq_u8_p128(hi));
            if EOR3 {
                veor3q_u8(lo, hi, add)
            } else {
                veorq_u8(veorq_u8(lo, hi), add)
            }
        }
    }
    #[inline(always)]
    unsafe fn mul(a: u64, b: u64) -> (u64, u64) {
        let p = unsafe { vmull_p64(a, b) };
        (p as u64, (p >> 64) as u64)
    }
    #[inline(always)]
    unsafe fn crc32c_u64(state: u32, v: u64) -> u32 {
        unsafe { __crc32cd(state, v) }
    }
    #[inline(always)]
    unsafe fn crc32c_u8(state: u32, v: u8) -> u32 {
        unsafe { __crc32cb(state, v) }
    }
}

macro_rules! kernel {
    ($crc64:ident, $crc32c:ident, $fused:ident, $lanes:literal, $eor3:literal, $features:literal) => {
        #[target_feature(enable = $features)]
        unsafe fn $crc64(state: u64, data: &[u8]) -> u64 {
            unsafe { fold::crc64::<Lanes<Pmull<$eor3>, $lanes>>(state, data) }
        }
        #[target_feature(enable = $features)]
        unsafe fn $crc32c(state: u32, data: &[u8]) -> u32 {
            unsafe { fold::crc32c::<Lanes<Pmull<$eor3>, $lanes>>(state, data) }
        }
        kernel!($fused, $lanes, 2, $eor3, $features);
    };
    ($fused:ident, $lanes:literal, $words:literal, $eor3:literal, $features:literal) => {
        #[target_feature(enable = $features)]
        unsafe fn $fused(state: u32, data: &[u8]) -> u32 {
            unsafe { fold::crc32c_fused::<Pmull<$eor3>, $lanes, $words>(state, data) }
        }
    };
}
kernel!(e12_64, e12_32, e12_fused, 12, true, "neon,aes,crc,sha3");
kernel!(e8_64, e8_32, e8_fused, 8, true, "neon,aes,crc,sha3");
kernel!(p12_64, p12_32, p12_fused, 12, false, "neon,aes,crc");
kernel!(p8_64, p8_32, p8_fused, 8, false, "neon,aes,crc");
kernel!(e12_fused3, 12, 3, true, "neon,aes,crc,sha3");
kernel!(e9_fused2, 9, 2, true, "neon,aes,crc,sha3");
kernel!(p12_fused3, 12, 3, false, "neon,aes,crc");

fn has_pmull() -> bool {
    is_aarch64_feature_detected!("aes") && is_aarch64_feature_detected!("crc")
}
fn has_eor3() -> bool {
    has_pmull() && is_aarch64_feature_detected!("sha3")
}

/// In order of preference; the table kernel is last. A name gives the
/// folding lanes, then for CRC-32C the words each of the three CRC
/// instruction streams reads beside them ("+3xS"), if any.
pub(crate) static KERNELS: [Kernel; 12] = [
    Kernel::new("pmull-eor3 x12 +3x3", has_eor3, e12_64, e12_fused3),
    Kernel::new("pmull x12 +3x3", has_pmull, p12_64, p12_fused3),
    Kernel::new("pmull-eor3 x12 +3x2", has_eor3, e12_64, e12_fused),
    Kernel::new("pmull x12 +3x2", has_pmull, p12_64, p12_fused),
    Kernel::new("pmull-eor3 x9 +3x2", has_eor3, e12_64, e9_fused2),
    Kernel::new("pmull-eor3 x8 +3x2", has_eor3, e8_64, e8_fused),
    Kernel::new("pmull x8 +3x2", has_pmull, p8_64, p8_fused),
    Kernel::new("pmull-eor3 x12", has_eor3, e12_64, e12_32),
    Kernel::new("pmull x12", has_pmull, p12_64, p12_32),
    Kernel::new("pmull-eor3 x8", has_eor3, e8_64, e8_32),
    Kernel::new("pmull x8", has_pmull, p8_64, p8_32),
    Kernel::TABLE,
];

/// The index in `KERNELS` of the kernel to use on this processor.
#[inline]
pub(crate) fn pick() -> usize {
    if has_eor3() {
        0
    } else if has_pmull() {
        1
    } else {
        KERNELS.len() - 1
    }
}
