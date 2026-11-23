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

/// A register of one or more 16-byte blocks, each folded on its own. The
/// main loop is written once against this, for every register width.
pub(crate) trait Reg {
    type C: Clmul;
    type R: Copy;
    /// Blocks in one register.
    const BLOCKS: usize;
    unsafe fn load(p: *const u8) -> Self::R;
    /// The constant that moves every block of a register `blocks` blocks on.
    unsafe fn key(keys: &Keys, blocks: usize) -> Self::R;
    /// Each block of `v` times its half of `k`, plus `add`.
    unsafe fn fold(v: Self::R, k: Self::R, add: Self::R) -> Self::R;
    /// `v` with `acc`, moved one block on, folded into its first block.
    unsafe fn enter(acc: <Self::C as Clmul>::V, v: Self::R, keys: &Keys) -> Self::R;
    /// The blocks of `v` as one block standing for all of them.
    unsafe fn narrow(v: Self::R, keys: &Keys) -> <Self::C as Clmul>::V;
}

/// A register of one block.
pub(crate) struct One<C>(core::marker::PhantomData<C>);

impl<C: Clmul> Reg for One<C> {
    type C = C;
    type R = C::V;
    const BLOCKS: usize = 1;

    #[inline(always)]
    unsafe fn load(p: *const u8) -> C::V {
        unsafe { C::load(p) }
    }
    #[inline(always)]
    unsafe fn key(keys: &Keys, blocks: usize) -> C::V {
        unsafe { C::load(keys[blocks].as_ptr().cast()) }
    }
    #[inline(always)]
    unsafe fn fold(v: C::V, k: C::V, add: C::V) -> C::V {
        unsafe { C::fold(v, k, add) }
    }
    #[inline(always)]
    unsafe fn enter(acc: C::V, v: C::V, keys: &Keys) -> C::V {
        unsafe { C::fold(acc, Self::key(keys, 1), v) }
    }
    #[inline(always)]
    unsafe fn narrow(v: C::V, _keys: &Keys) -> C::V {
        v
    }
}

/// `M` registers of kind `G` folded side by side.
pub(crate) struct Wide<G, const M: usize>(core::marker::PhantomData<G>);

/// `N` lanes of 128 bits.
pub(crate) type Lanes<C, const N: usize> = Wide<One<C>, N>;

impl<G: Reg, const M: usize> Bulk for Wide<G, M> {
    type C = G::C;

    #[inline(always)]
    unsafe fn bulk<'a>(
        acc: <G::C as Clmul>::V,
        rest: &'a [u8],
        keys: &Keys,
    ) -> (<G::C as Clmul>::V, &'a [u8]) {
        let width = 16 * G::BLOCKS;
        if rest.len() < 2 * M * width {
            return (acc, rest);
        }
        // SAFETY: every read is below `rest.len()`: the first `M` registers
        // by the check above, later ones by the loop condition.
        unsafe {
            let mut p = rest.as_ptr();
            let mut left = rest.len();
            let km = G::key(keys, M * G::BLOCKS);
            // Written so the compiler unrolls it and keeps every register in
            // a register; a loop that skips the first is left as a loop, and
            // they then live in memory.
            let mut v = [G::load(p); M];
            for (i, r) in v.iter_mut().enumerate() {
                *r = G::load(p.add(i * width));
            }
            v[0] = G::enter(acc, v[0], keys);
            p = p.add(M * width);
            left -= M * width;
            while left >= M * width {
                for (i, r) in v.iter_mut().enumerate() {
                    *r = G::fold(*r, km, G::load(p.add(i * width)));
                }
                p = p.add(M * width);
                left -= M * width;
            }
            let narrowed = G::narrow(collapse::<G, M>(&v, keys), keys);
            (narrowed, &rest[rest.len() - left..])
        }
    }
}

/// `M` registers into one. Each but the last is moved to the last one's
/// place by a constant of its own, so the multiplications do not wait for
/// one another. Folding them one into the next instead costs `M - 1`
/// multiplications in a row, which is most of the time on a short input.
#[inline(always)]
unsafe fn collapse<G: Reg, const M: usize>(v: &[G::R; M], keys: &Keys) -> G::R {
    // SAFETY: the caller enabled the instructions `G` uses.
    unsafe {
        let mut x = v[M - 1];
        for (i, r) in v.iter().enumerate().take(M - 1) {
            x = G::fold(*r, G::key(keys, (M - 1 - i) * G::BLOCKS), x);
        }
        x
    }
}

/// `K` blocks at `p` taken into `acc` at once. Every block, and `acc`, is
/// moved to the last block's place by a constant of its own, as in
/// `collapse`, so no multiplication waits for another. Two sums are kept
/// so that adding the products does not form one chain either. `K` is a
/// constant so that the loop is unrolled.
#[inline(always)]
unsafe fn take<C: Clmul, const K: usize>(acc: C::V, p: *const u8, keys: &Keys) -> C::V {
    // SAFETY: the caller enabled the instructions `C` uses and `K` blocks
    // are readable at `p`.
    unsafe {
        let key = |j: usize| C::load(keys[j].as_ptr().cast());
        let mut sums = [
            C::fold(acc, key(K), C::load(p.add((K - 1) * 16))),
            C::set(0, 0),
        ];
        for i in 0..K - 1 {
            sums[i % 2] = C::fold(C::load(p.add(i * 16)), key(K - 1 - i), sums[i % 2]);
        }
        C::xor(sums[0], sums[1])
    }
}

/// The whole blocks the widest stage left, or all of a short input: eight
/// at a time, then four, then the last few one after another.
#[inline(always)]
unsafe fn tail<C: Clmul>(mut acc: C::V, rest: &[u8], keys: &Keys) -> C::V {
    // SAFETY: the caller enabled the instructions `C` uses; `left` counts
    // the bytes still unread at `p`.
    unsafe {
        let mut p = rest.as_ptr();
        let mut left = rest.len();
        while left >= 8 * 16 {
            acc = take::<C, 8>(acc, p, keys);
            p = p.add(8 * 16);
            left -= 8 * 16;
        }
        if left >= 4 * 16 {
            acc = take::<C, 4>(acc, p, keys);
            p = p.add(4 * 16);
            left -= 4 * 16;
        }
        let k1 = C::load(keys[1].as_ptr().cast());
        while left >= 16 {
            acc = C::fold(acc, k1, C::load(p));
            p = p.add(16);
            left -= 16;
        }
        acc
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
        tail::<B::C>(acc, rest, keys)
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
        let (lo, hi) = <B::C>::get(body::<B>(u64::from(state), data, &KEYS32));
        // The processor's CRC instruction does the final reduction.
        <B::C>::crc32c_u64(<B::C>::crc32c_u64(0, lo), hi)
    }
}

/// The shortest input the interleaved kernel takes on, for `folded` bytes
/// in the registers and `words` words in each CRC stream per round. Joining
/// its four results costs a few dozen multiplications, so shorter inputs
/// are folded only.
pub(crate) const fn fused_min(folded: usize, words: usize) -> usize {
    16 + 16 * (folded + 24 * words)
}

/// CRC-32C with the CRC instruction working beside the multiplier.
///
/// The two use different parts of the processor, so the input is cut into a
/// region for `M` folding registers and three regions for the CRC
/// instruction, all four read in one loop. Each round takes one register's
/// worth for each of the `M` and `S` eight-byte words for each of the three
/// streams. The four results are then joined, each moved past the regions
/// that follow it.
#[inline(always)]
pub(crate) unsafe fn crc32c_fused<G: Reg, const M: usize, const S: usize>(
    state: u32,
    data: &[u8],
) -> u32 {
    let width = 16 * G::BLOCKS;
    let step = M * width + 24 * S;
    // SAFETY: the caller enabled the instructions `G` uses. Reads stay inside
    // the four regions, each `rounds` rounds long.
    unsafe {
        if data.len() < fused_min(M * width, S) {
            return crc32c::<Wide<G, M>>(state, data);
        }
        let (acc, rest) = head::<G::C>(u64::from(state), data, &KEYS32);
        let rounds = rest.len() / step;
        let stream = 8 * S * rounds;
        let (folded, rest) = rest.split_at(M * width * rounds);
        let (s1, rest) = rest.split_at(stream);
        let (s2, rest) = rest.split_at(stream);
        let (s3, after) = rest.split_at(stream);

        let km = G::key(&KEYS32, M * G::BLOCKS);
        let mut p = folded.as_ptr();
        let mut q = [s1.as_ptr(), s2.as_ptr(), s3.as_ptr()];
        let mut c = [0u32; 3];
        let mut v = [G::load(p); M];
        for (i, r) in v.iter_mut().enumerate() {
            *r = G::load(p.add(i * width));
        }
        v[0] = G::enter(acc, v[0], &KEYS32);
        for round in 0..rounds {
            if round != 0 {
                for (i, r) in v.iter_mut().enumerate() {
                    *r = G::fold(*r, km, G::load(p.add(i * width)));
                }
            }
            for word in 0..S {
                for (part, ptr) in c.iter_mut().zip(&q) {
                    let w = ptr.add(word * 8).cast::<u64>().read_unaligned();
                    *part = <G::C>::crc32c_u64(*part, u64::from_le(w));
                }
            }
            p = p.add(M * width);
            for ptr in &mut q {
                *ptr = ptr.add(S * 8);
            }
        }
        let narrowed = G::narrow(collapse::<G, M>(&v, &KEYS32), &KEYS32);
        let (lo, hi) = <G::C>::get(narrowed);
        let mut crc = <G::C>::crc32c_u64(<G::C>::crc32c_u64(0, lo), hi);

        // x^(8 * stream - 33): a multiplication by it, through the CRC
        // instruction, moves a value past one stream.
        let mut past = X_INV33;
        let mut bits = 8 * stream as u64;
        while bits != 0 {
            let k = SHIFT32[bits.trailing_zeros() as usize];
            past = <G::C>::crc32c_u64(0, <G::C>::mul(u64::from(past), u64::from(k)).0);
            bits &= bits - 1;
        }
        for part in c {
            crc = <G::C>::crc32c_u64(0, <G::C>::mul(u64::from(crc), u64::from(past)).0) ^ part;
        }
        crc32c::<Wide<G, M>>(crc, after)
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
