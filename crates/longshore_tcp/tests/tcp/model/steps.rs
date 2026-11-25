// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The steps of a run, made from its seed.

/// What a step does. Which connection, listener or length it does it
/// with is taken from the step's two numbers when it runs, against what
/// exists then: the same list means the same run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Poll both sets, turn the backend, route its completions.
    Turn,
    PullEvents,
    PullFrames,
    Send,
    Connect,
    Listen,
    Close,
    CloseListener,
    Shutdown,
    /// Release a frame held from an earlier pull.
    Release,
}

/// One step of a run.
#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub(crate) kind: Kind,
    /// On the server set; otherwise the client set.
    pub(crate) server: bool,
    pub(crate) a: u32,
    pub(crate) b: u32,
}

/// A small generator of its own, so that the list depends on nothing else.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) const fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// How often each kind comes, out of the sum of these.
const WEIGHTS: [(Kind, u64); 10] = [
    (Kind::Turn, 30),
    (Kind::PullEvents, 14),
    (Kind::PullFrames, 14),
    (Kind::Send, 24),
    (Kind::Connect, 4),
    (Kind::Listen, 3),
    (Kind::Close, 2),
    (Kind::CloseListener, 2),
    (Kind::Shutdown, 2),
    (Kind::Release, 5),
];

/// The `count` steps of the run with `seed`.
pub fn generate(seed: u64, count: usize) -> Vec<Step> {
    let total: u64 = WEIGHTS.iter().map(|(_, w)| *w).sum();
    let mut rng = Rng::new(seed);
    let mut steps = Vec::with_capacity(count);
    for _ in 0..count {
        let mut pick = rng.next() % total;
        let mut kind = Kind::Turn;
        for (k, w) in WEIGHTS {
            if pick < w {
                kind = k;
                break;
            }
            pick -= w;
        }
        let bits = rng.next();
        steps.push(Step {
            kind,
            server: bits & 1 == 1,
            a: (bits >> 1) as u32,
            b: (bits >> 33) as u32,
        });
    }
    steps
}

/// The bytes one step is read from.
pub const STEP_BYTES: usize = 9;

/// A run from bytes, as a fuzzer makes them: the first eight are the
/// simulator's seed, and each nine after them a step, up to `most` of
/// them. The first byte of a step says what it does (its high bits, by
/// the order of [`Kind`]) and on which side (its low bit); the others
/// are its two numbers. `None` for fewer than eight bytes.
pub fn from_bytes(data: &[u8], most: usize) -> Option<(u64, Vec<Step>)> {
    const KINDS: [Kind; 10] = [
        Kind::Turn,
        Kind::PullEvents,
        Kind::PullFrames,
        Kind::Send,
        Kind::Connect,
        Kind::Listen,
        Kind::Close,
        Kind::CloseListener,
        Kind::Shutdown,
        Kind::Release,
    ];
    let (seed, rest) = data.split_first_chunk::<8>()?;
    let steps = rest
        .as_chunks::<STEP_BYTES>()
        .0
        .iter()
        .take(most)
        .map(|s| Step {
            kind: KINDS[usize::from(s[0] >> 1) % KINDS.len()],
            server: s[0] & 1 == 1,
            a: u32::from_le_bytes([s[1], s[2], s[3], s[4]]),
            b: u32::from_le_bytes([s[5], s[6], s[7], s[8]]),
        })
        .collect();
    Some((u64::from_le_bytes(*seed), steps))
}
