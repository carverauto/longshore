// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The constants that join the streams of a fused CRC-32C kernel, checked
//! against arithmetic written here and against the table kernel.
//!
//! Conventions (see `keys.rs`): a 32-bit value is a polynomial over GF(2)
//! with bit `31 - i` holding the coefficient of `x^i`, so `1 << 31` is 1 and
//! a shift right by one bit is a multiplication by `x`.
//!
//! `SHIFT32[i]` is documented as `x^(2^i - 33)` modulo the polynomial. The
//! exponent is negative for small `i`, so the entries are not compared with
//! a power directly. Multiplying an entry by `x^33` (what the CRC
//! instruction adds to a carry-less product in `fold::crc32c_fused`) gives
//! `e(i) = x^(2^i)`, and three facts together fix all 64 entries:
//!
//! 1. `e(0) = x`.
//! 2. `e(i + 1) = e(i)^2`, since `x^(2^(i + 1)) = (x^(2^i))^2`.
//! 3. A CRC state passed over a zero byte is multiplied by `x^8`: the byte
//!    adds nothing, and each of its eight bits shifts the register once and
//!    reduces it. So over `n` zero bytes the table kernel multiplies its
//!    state by `x^(8n)`, which is `e(i)` when `8n = 2^i`.

use crate::keys::{POLY32, SHIFT32, X_INV33};

/// `v * x` modulo the polynomial.
fn times_x(v: u32) -> u32 {
    (v >> 1) ^ if v & 1 == 1 { POLY32 } else { 0 }
}

/// `a * b` modulo the polynomial, by Horner's rule from the highest power of
/// `b` down: not the method `keys.rs` uses.
fn mul(a: u32, b: u32) -> u32 {
    let mut r = 0;
    for i in 0..32 {
        // Bit `i` is the coefficient of `x^(31 - i)`: highest power first.
        r = times_x(r);
        if (b >> i) & 1 == 1 {
            r ^= a;
        }
    }
    r
}

const ONE: u32 = 1 << 31;
const X: u32 = 1 << 30;

fn x33() -> u32 {
    (0..33).fold(ONE, |v, _| times_x(v))
}

/// `SHIFT32[i]` with the CRC instruction's `x^33` applied: `x^(2^i)`.
fn e(i: usize) -> u32 {
    mul(SHIFT32[i], x33())
}

#[test]
fn the_multiplication_written_here_has_one_and_x() {
    for v in [ONE, X, POLY32, 0xdead_beef, 0x0000_0001] {
        assert_eq!(mul(v, ONE), v);
        assert_eq!(mul(ONE, v), v);
        assert_eq!(mul(v, X), times_x(v));
        assert_eq!(mul(X, v), times_x(v));
    }
    assert_eq!(mul(0x1234_5678, 0x9abc_def0), mul(0x9abc_def0, 0x1234_5678));
}

#[test]
fn x_inv33_undoes_the_crc_instructions_x33() {
    assert_eq!(mul(X_INV33, x33()), ONE);
}

#[test]
fn every_shift_entry_is_the_square_of_the_one_before() {
    assert_eq!(e(0), X, "entry 0 moves a value one bit");
    for i in 0..63 {
        assert_eq!(e(i + 1), mul(e(i), e(i)), "entry {}", i + 1);
    }
}

#[test]
fn shift_entries_agree_with_the_table_kernel_over_zero_bytes() {
    // Entry 3 moves a value 8 bits, which is one zero byte; entry `i` is
    // `2^(i - 3)` of them. Up to entry 23, a mebibyte of zeros.
    let zeros = vec![0u8; 1 << 20];
    for state in [ONE, X, 0xffff_ffff, 0x89ab_cdef, 0x0000_0001] {
        for i in 3..=23 {
            let n = 1usize << (i - 3);
            assert_eq!(
                crate::table::crc32c(state, &zeros[..n]),
                mul(state, e(i)),
                "state {state:#x} over {n} zero bytes, entry {i}"
            );
        }
    }
}
