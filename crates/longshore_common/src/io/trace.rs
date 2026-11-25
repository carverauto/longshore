// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The hash a deterministic run is known by: every event folded into one
//! number, the same for the same seed and the same calls.

/// What a trace starts from.
pub(crate) const START: u64 = 0xcbf2_9ce4_8422_2325;

/// Fold `parts` into `trace`, in order.
#[inline]
pub(crate) fn fold(trace: &mut u64, parts: &[u64]) {
    for part in parts {
        *trace = (*trace ^ part).wrapping_mul(0x0000_0100_0000_01b3);
    }
}
