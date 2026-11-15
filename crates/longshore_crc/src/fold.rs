// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Checksums by carry-less multiplication, written once for every
//! processor that has one.
//!
//! The input is read as 16-byte blocks. A 128-bit value that stands for all
//! input so far is "folded" across the next block: each half is multiplied
//! by a constant that moves it 128 bits further along, modulo the
//! polynomial, and the block is added. Several independent lanes run side by
//! side so the processor's multipliers stay busy, then collapse into one.
//! The last 128 bits are reduced to the checksum's width.

use crate::keys::{Keys, KEYS32, KEYS64, MU64, POLY64, SHIFT32, X_INV33};

/// What a kernel needs from the processor. Every method is a few
/// instructions and must be inlined into a function that enables them.
pub(crate) trait Clmul {
    type V: Copy;
    unsafe fn load(p: *const u8) -> Self::V;
    unsafe fn set(lo: u64, hi: u64) -> Self::V;
    unsafe fn get(v: Self::V) -> (u64, u64);
    unsafe fn xor(a: Self::V, b: Self::V) -> Self::V;
    /// `v.lo * k.lo + v.hi * k.hi + add`, carry-less.
    unsafe fn fold(v: Self::V, k: Self::V, add: Self::V) -> Self::V;
    /// The 128-bit carry-less product, low half first.
    unsafe fn mul(a: u64, b: u64) -> (u64, u64);
    unsafe fn crc32c_u64(state: u32, v: u64) -> u32;
    unsafe fn crc32c_u8(state: u32, v: u8) -> u32;
}

/// The widest stage of a kernel: consumes whole blocks from the front of
/// `rest` and returns what it left.
pub(crate) trait Bulk {
    type C: Clmul;
    unsafe fn bulk<'a>(
        acc: <Self::C as Clmul>::V,
        rest: &'a [u8],
        keys: &Keys,
    ) -> (<Self::C as Clmul>::V, &'a [u8]);
}

/// `N` lanes of 128 bits.
pub(crate) struct Lanes<C, const N: usize>(core::marker::PhantomData<C>);

impl<C: Clmul, const N: usize> Bulk for Lanes<C, N> {
    type C = C;

    #[inline(always)]
    unsafe fn bulk<'a>(acc: C::V, rest: &'a [u8], keys: &Keys) -> (C::V, &'a [u8]) {
        if rest.len() < 2 * N * 16 {
            return (acc, rest);
        }
        // SAFETY: every read is below `rest.len()`: the first `N` blocks by
        // the check above, later ones by the loop condition.
        unsafe {
            let mut p = rest.as_ptr();
            let mut left = rest.len();
            let k1 = C::load(keys[1].as_ptr().cast());
            let kn = C::load(keys[N].as_ptr().cast());
            // Written so the compiler unrolls it and keeps every lane in a
            // register; a loop that skips the first lane is left as a loop,
            // and the lanes then live in memory.
            let mut lane = [acc; N];
            for (i, l) in lane.iter_mut().enumerate() {
                *l = C::load(p.add(i * 16));
            }
            // One block further along, `acc` lines up with the first lane.
            lane[0] = C::fold(acc, k1, lane[0]);
            p = p.add(N * 16);
            left -= N * 16;
            while left >= N * 16 {
                for (i, l) in lane.iter_mut().enumerate() {
                    *l = C::fold(*l, kn, C::load(p.add(i * 16)));
                }
                p = p.add(N * 16);
                left -= N * 16;
            }
            let mut acc = lane[0];
            for l in &lane[1..] {
                acc = C::fold(acc, k1, *l);
            }
            (acc, &rest[rest.len() - left..])
        }
    }
}

/// Takes the state and the first bytes of `data` (at least 16) into one
/// 128-bit value. What it leaves is a whole number of blocks.
#[inline(always)]
unsafe fn head<'a, C: Clmul>(state: u64, data: &'a [u8], keys: &Keys) -> (C::V, &'a [u8]) {
    let odd = data.len() % 16;
    // SAFETY: `data` holds at least 16 bytes, and `buf` 32.
    unsafe {
        if odd == 0 {
            let first = C::xor(C::load(data.as_ptr()), C::set(state, 0));
            return (first, &data[16..]);
        }
        // Zeros in front of a message do not change a CRC whose state is
        // zero, and the state is the same as an addition to the first eight
        // bytes. So pad the front to a whole block and fold two blocks.
        let mut buf = [0u8; 32];
        buf[16 - odd..].copy_from_slice(&data[..16 + odd]);
        for (b, s) in buf[16 - odd..].iter_mut().zip(state.to_le_bytes()) {
            *b ^= s;
        }
        let k1 = C::load(keys[1].as_ptr().cast());
        let acc = C::fold(C::load(buf.as_ptr()), k1, C::load(buf.as_ptr().add(16)));
        (acc, &data[16 + odd..])
    }
}

/// Everything between the first block and the final reduction.
#[inline(always)]
unsafe fn body<B: Bulk>(state: u64, data: &[u8], keys: &Keys) -> <B::C as Clmul>::V {
    // SAFETY: `head` leaves whole blocks and each stage keeps it so.
    unsafe {
        let (acc, rest) = head::<B::C>(state, data, keys);
        let (acc, rest) = B::bulk(acc, rest, keys);
        let (mut acc, rest) = Lanes::<B::C, 4>::bulk(acc, rest, keys);
        let k1 = <B::C>::load(keys[1].as_ptr().cast());
        for block in rest.as_chunks::<16>().0 {
            acc = <B::C>::fold(acc, k1, <B::C>::load(block.as_ptr()));
        }
        acc
    }
}

/// Advances a CRC-64/NVME state over `data`.
#[inline(always)]
pub(crate) unsafe fn crc64<B: Bulk>(state: u64, data: &[u8]) -> u64 {
    if data.len() < 16 {
        return crate::table::crc64(state, data);
    }
    // SAFETY: the caller enabled the instructions `B` uses.
    unsafe {
        let (lo, hi) = <B::C>::get(body::<B>(state, data, &KEYS64));
        // 128 bits to 128 bits that stand for the value times x^64: the low
        // half moves up by x^128, the high half is already in place.
        let (t_lo, t_hi) = <B::C>::mul(lo, KEYS64[1][1]);
        let (w_lo, w_hi) = (t_lo ^ hi, t_hi);
        // Barrett reduction of the low half. A carry-less product of two
        // reflected words comes out one bit low, hence the shifts.
        let (m_lo, _) = <B::C>::mul(w_lo, MU64);
        let quotient = w_lo ^ (m_lo << 1);
        let (r_lo, r_hi) = <B::C>::mul(quotient, POLY64);
        w_hi ^ ((r_lo >> 63) | (r_hi << 1))
    }
}

/// Advances a CRC-32C state over `data`.
#[inline(always)]
pub(crate) unsafe fn crc32c<B: Bulk>(state: u32, data: &[u8]) -> u32 {
    // SAFETY: the caller enabled the instructions `B` uses.
    unsafe {
        if data.len() < 32 {
            return crc32c_short::<B::C>(state, data);
        }
        let (lo, hi) = <B::C>::get(body::<B>(state as u64, data, &KEYS32));
        // The processor's CRC instruction does the final reduction.
        <B::C>::crc32c_u64(<B::C>::crc32c_u64(0, lo), hi)
    }
}

/// CRC-32C with the CRC instruction working beside the multiplier.
///
/// The two use different parts of the processor, so the input is cut into a
/// region for `N` folding lanes and three regions for the CRC instruction,
/// all four read in one loop. Each iteration takes `N` blocks for the lanes
/// and `S` eight-byte words for each of the three streams. The four results
/// are then joined, each moved past the regions that follow it.
#[inline(always)]
pub(crate) unsafe fn crc32c_fused<C: Clmul, const N: usize, const S: usize>(
    state: u32,
    data: &[u8],
) -> u32 {
    let step = 16 * N + 24 * S;
    // SAFETY: the caller enabled the instructions `C` uses. Reads stay inside
    // the four regions, each `rounds` iterations long.
    unsafe {
        // Joining the four results costs a few dozen multiplications, so
        // short inputs are folded only.
        if data.len() < 16 + 16 * step {
            return crc32c::<Lanes<C, N>>(state, data);
        }
        let (acc, rest) = head::<C>(state as u64, data, &KEYS32);
        let rounds = rest.len() / step;
        let stream = 8 * S * rounds;
        let (folded, rest) = rest.split_at(16 * N * rounds);
        let (s1, rest) = rest.split_at(stream);
        let (s2, rest) = rest.split_at(stream);
        let (s3, tail) = rest.split_at(stream);

        let k1 = C::load(KEYS32[1].as_ptr().cast());
        let kn = C::load(KEYS32[N].as_ptr().cast());
        let mut p = folded.as_ptr();
        let mut q = [s1.as_ptr(), s2.as_ptr(), s3.as_ptr()];
        let mut c = [0u32; 3];
        let mut lane = [acc; N];
        for (i, l) in lane.iter_mut().enumerate() {
            *l = C::load(p.add(i * 16));
        }
        lane[0] = C::fold(acc, k1, lane[0]);
        for round in 0..rounds {
            if round != 0 {
                for (i, l) in lane.iter_mut().enumerate() {
                    *l = C::fold(*l, kn, C::load(p.add(i * 16)));
                }
            }
            for word in 0..S {
                for (c, q) in c.iter_mut().zip(&q) {
                    let v = q.add(word * 8).cast::<u64>().read_unaligned();
                    *c = C::crc32c_u64(*c, u64::from_le(v));
                }
            }
            p = p.add(N * 16);
            for q in &mut q {
                *q = q.add(S * 8);
            }
        }
        let mut acc = lane[0];
        for l in &lane[1..] {
            acc = C::fold(acc, k1, *l);
        }
        let (lo, hi) = C::get(acc);
        let mut crc = C::crc32c_u64(C::crc32c_u64(0, lo), hi);

        // x^(8 * stream - 33): a multiplication by it, through the CRC
        // instruction, moves a value past one stream.
        let mut past = X_INV33;
        let mut bits = 8 * stream as u64;
        while bits != 0 {
            let k = SHIFT32[bits.trailing_zeros() as usize];
            past = C::crc32c_u64(0, C::mul(past as u64, k as u64).0);
            bits &= bits - 1;
        }
        for c in c {
            crc = C::crc32c_u64(0, C::mul(crc as u64, past as u64).0) ^ c;
        }
        crc32c::<Lanes<C, 4>>(crc, tail)
    }
}

#[inline(always)]
unsafe fn crc32c_short<C: Clmul>(mut state: u32, data: &[u8]) -> u32 {
    // SAFETY: the caller enabled the CRC instruction.
    unsafe {
        let (words, tail) = data.as_chunks::<8>();
        for w in words {
            state = C::crc32c_u64(state, u64::from_le_bytes(*w));
        }
        for &b in tail {
            state = C::crc32c_u8(state, b);
        }
        state
    }
}
