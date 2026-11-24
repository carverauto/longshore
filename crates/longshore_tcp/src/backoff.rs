// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Delays between reconnect attempts: exponential, capped, with jitter.
//!
//! Many connections that lose one peer at the same moment would otherwise
//! all redial it at the same moments. The jitter comes from a seeded
//! generator, so a simulation with the same seed redials at the same times.

/// The delay before each attempt to reconnect.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    base_ns: u64,
    max_ns: u64,
    /// Share of each delay that is random, in thousandths.
    jitter_per_mille: u32,
    attempt: u32,
    state: u64,
}

impl Backoff {
    /// Delays of `base_ns`, doubling per attempt up to `max_ns`, each
    /// shortened by a random share of up to `jitter_per_mille` thousandths
    /// (at most 1000). `seed` fixes the random shares.
    pub const fn new(base_ns: u64, max_ns: u64, jitter_per_mille: u32, seed: u64) -> Backoff {
        assert!(base_ns > 0 && max_ns >= base_ns && jitter_per_mille <= 1000);
        Backoff {
            base_ns,
            max_ns,
            jitter_per_mille,
            attempt: 0,
            // A xorshift state must not be zero.
            state: seed | 1,
        }
    }

    /// The delay before the next attempt; later calls give longer ones.
    pub fn next_delay_ns(&mut self) -> u64 {
        let full = self
            .base_ns
            .checked_shl(self.attempt)
            .filter(|d| d >> self.attempt == self.base_ns)
            .map_or(self.max_ns, |d| d.min(self.max_ns));
        self.attempt = self.attempt.saturating_add(1);
        // xorshift64
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        let span = full / 1000 * u64::from(self.jitter_per_mille);
        let cut = if span == 0 { 0 } else { x % (span + 1) };
        full - cut
    }

    /// The connection is up: the next failure starts from the base delay.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Attempts made since the last reset.
    pub const fn attempts(&self) -> u32 {
        self.attempt
    }
}
