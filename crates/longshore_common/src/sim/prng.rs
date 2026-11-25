// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! xoshiro256**, seeded with splitmix64.

/// The simulator's pseudo-random numbers: the same seed gives the same sequence
/// on every platform. Not for cryptography.
#[derive(Clone)]
pub struct Prng {
    s: [u64; 4],
}

impl Prng {
    /// The sequence that `seed` gives.
    pub fn new(seed: u64) -> Prng {
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^ (x >> 31)
        };
        Prng {
            s: [next(), next(), next(), next()],
        }
    }

    /// The next 64 bits of the sequence.
    pub fn next_u64(&mut self) -> u64 {
        let r = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }

    /// Uniform in `0..n`. `n` must be non-zero.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// True with probability `per_million / 1_000_000`.
    pub fn chance(&mut self, per_million: u32) -> bool {
        per_million != 0 && self.below(1_000_000) < u64::from(per_million)
    }
}
