// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Constants for both checksums, computed from the polynomials when the
//! crate is compiled.
//!
//! Both are reflected CRCs. A 64-bit word stands for a polynomial with bit
//! `i` holding the coefficient of `x^(63 - i)`; a 32-bit CRC-32C value
//! occupies the high half of such a word.

/// CRC-64/NVME polynomial, reflected.
pub(crate) const POLY64: u64 = 0x9a6c_9329_ac4b_c9b5;
/// CRC-32C (Castagnoli) polynomial, reflected.
pub(crate) const POLY32: u32 = 0x82f6_3b78;

/// The longest distance a kernel folds across, in 16-byte blocks.
pub(crate) const MAX_BLOCKS: usize = 32;

/// Entry `j` folds a 128-bit value across `j` blocks: `[x^(d + 63), x^(d - 1)]`
/// modulo the polynomial, with `d = 128 * j`. Entry 0 is unused.
pub(crate) type Keys = [[u64; 2]; MAX_BLOCKS + 1];

const fn keys(wide: bool) -> Keys {
    let mut t = [[0u64; 2]; MAX_BLOCKS + 1];
    // `v` is x^n, starting from x^0.
    let mut v = 1u64 << 63;
    let mut n = 0;
    while n <= MAX_BLOCKS * 128 + 63 {
        if (n + 1) % 128 == 0 {
            t[(n + 1) / 128][1] = v;
        }
        if n >= 128 + 63 && (n - 63) % 128 == 0 {
            t[(n - 63) / 128][0] = v;
        }
        v = if wide {
            (v >> 1) ^ (if v & 1 == 1 { POLY64 } else { 0 })
        } else {
            let w = (v >> 32) as u32;
            (((w >> 1) ^ (if w & 1 == 1 { POLY32 } else { 0 })) as u64) << 32
        };
        n += 1;
    }
    t
}

pub(crate) static KEYS64: Keys = keys(true);
pub(crate) static KEYS32: Keys = keys(false);

/// The low 64 coefficients of `floor(x^128 / P)` for CRC-64/NVME, reflected.
/// The quotient's `x^64` term is always one and is applied separately.
pub(crate) const MU64: u64 = {
    let poly = POLY64.reverse_bits();
    // Long division of a one followed by 128 zeros, most significant first.
    let (mut rem, mut quotient) = (0u64, 0u128);
    let mut i = 0;
    while i <= 128 {
        let top = rem >> 63;
        rem = (rem << 1) | (i == 0) as u64;
        quotient = (quotient << 1) | top as u128;
        if top == 1 {
            rem ^= poly;
        }
        i += 1;
    }
    (quotient as u64).reverse_bits()
};

/// `a * b` modulo the CRC-32C polynomial, on 32-bit reflected values.
const fn mul32(a: u32, mut b: u32) -> u32 {
    let mut r = 0;
    let mut i = 0;
    while i < 32 {
        if (a >> (31 - i)) & 1 == 1 {
            r ^= b;
        }
        b = (b >> 1) ^ (if b & 1 == 1 { POLY32 } else { 0 });
        i += 1;
    }
    r
}

/// `x^-33` modulo the CRC-32C polynomial. Multiplying two 32-bit values
/// without carries and passing the product through the processor's CRC
/// instruction yields their product times `x^33`; this cancels that.
pub(crate) const X_INV33: u32 = {
    let mut v = 1u32 << 31;
    let mut i = 0;
    while i < 33 {
        // Division by x: undo the shift, and the reduction if there was one.
        v = if v >> 31 == 1 {
            ((v ^ POLY32) << 1) | 1
        } else {
            v << 1
        };
        i += 1;
    }
    v
};

/// Entry `i` is `x^(2^i - 33)` modulo the CRC-32C polynomial: with the CRC
/// instruction's `x^33`, a multiplication by it moves a value `2^i` bits.
pub(crate) static SHIFT32: [u32; 64] = {
    let mut t = [0u32; 64];
    let mut square = 1u32 << 30;
    let mut i = 0;
    while i < 64 {
        t[i] = mul32(square, X_INV33);
        square = mul32(square, square);
        i += 1;
    }
    t
};

const fn table64() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u64;
        let mut bit = 0;
        while bit < 8 {
            v = (v >> 1) ^ (if v & 1 == 1 { POLY64 } else { 0 });
            bit += 1;
        }
        t[i] = v;
        i += 1;
    }
    t
}

const fn table32() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u32;
        let mut bit = 0;
        while bit < 8 {
            v = (v >> 1) ^ (if v & 1 == 1 { POLY32 } else { 0 });
            bit += 1;
        }
        t[i] = v;
        i += 1;
    }
    t
}

pub(crate) static TABLE64: [u64; 256] = table64();
pub(crate) static TABLE32: [u32; 256] = table32();
