// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Locks, atomics, thread parking and interior-mutability cells that callers
//! and I/O threads meet on.
//!
//! Under ordinary builds these re-export the standard library's primitives.
//! Under `--cfg longshore_model` they swap to `loom`'s equivalents so the
//! model checker can explore every interleaving. `std::sync::Arc` is kept
//! standard everywhere because `Io::waker` and `std::task::Waker` require it.

#[cfg(not(longshore_model))]
pub use std::sync::atomic::{
    fence, AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
#[cfg(not(longshore_model))]
pub use std::sync::{Condvar, Mutex};

/// The guard (or other result) of a lock that was not poisoned.
///
/// A poisoned lock means a holder panicked part-way through an update;
/// going on would trust state left half-written, so the panic is passed on.
#[expect(
    clippy::expect_used,
    reason = "a poisoned lock holds half-written state; passing the holder's panic on is the only safe course"
)]
pub(crate) fn unpoisoned<G>(result: std::sync::LockResult<G>) -> G {
    result.expect("a thread panicked while holding this lock")
}
#[cfg(not(longshore_model))]
pub use std::thread::{current, park, yield_now, Thread};

/// Interior-mutability cell whose API matches `loom::cell::UnsafeCell`.
#[cfg(not(longshore_model))]
#[derive(Debug, Default)]
pub struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(longshore_model))]
impl<T> UnsafeCell<T> {
    #[inline]
    pub fn new(data: T) -> Self {
        Self(std::cell::UnsafeCell::new(data))
    }

    #[inline]
    pub fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    #[inline]
    pub fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}

#[cfg(longshore_model)]
pub use loom::cell::UnsafeCell;
#[cfg(longshore_model)]
pub use loom::sync::atomic::{
    fence, AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
#[cfg(longshore_model)]
pub use loom::sync::{Condvar, Mutex};
#[cfg(longshore_model)]
pub use loom::thread::{current, park, yield_now, Thread};
