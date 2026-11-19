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
pub use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(not(longshore_model))]
pub use std::sync::{Condvar, Mutex};
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
pub use loom::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(longshore_model)]
pub use loom::sync::{Condvar, Mutex};
#[cfg(longshore_model)]
pub use loom::thread::{current, park, yield_now, Thread};
