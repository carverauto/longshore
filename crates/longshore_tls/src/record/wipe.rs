// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Erasing key material held inline.

use core::mem::MaybeUninit;

/// Overwrite every byte of `v` with zero, so that the compiler cannot drop
/// the writes as dead stores.
pub(super) fn wipe_bytes<T>(v: &mut MaybeUninit<T>) {
    let bytes = v.as_mut_ptr().cast::<u8>();
    for i in 0..core::mem::size_of::<T>() {
        // SAFETY: `bytes` covers exactly `v`, which the caller owns.
        unsafe { bytes.add(i).write_volatile(0) };
    }
}

/// Whether any byte of `v` is non-zero. `v` must have been zeroed or fully
/// written: for tests of wiping.
#[cfg(test)]
pub(super) fn any_byte_set<T>(v: &MaybeUninit<T>) -> bool {
    // SAFETY: the callers hold `v` zeroed or wiped, so every byte is set.
    let bytes =
        unsafe { core::slice::from_raw_parts(v.as_ptr().cast::<u8>(), core::mem::size_of::<T>()) };
    bytes.iter().any(|b| *b != 0)
}
