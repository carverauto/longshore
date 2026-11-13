// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Makes "no heap allocation after initialization" a tested property.
//!
//! Install [`Guard`] as the global allocator in a test, simulation or
//! benchmark binary, then run steady-state code inside [`sealed`]. Any
//! allocation, reallocation or free on that thread while sealed aborts the
//! process. The seal is per thread so the test harness's own threads are free
//! to allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static SEALED: Cell<bool> = const { Cell::new(false) };
}

pub struct Guard;

extern "C" {
    fn write(fd: i32, buf: *const core::ffi::c_void, n: usize) -> isize;
    fn abort() -> !;
}

#[cold]
fn violation(what: &'static str) -> ! {
    // No formatting: that would allocate.
    let msg = b"alloc_guard: heap ";
    let tail = b" while sealed (allocation after initialization)\n";
    unsafe {
        write(2, msg.as_ptr().cast(), msg.len());
        write(2, what.as_ptr().cast(), what.len());
        write(2, tail.as_ptr().cast(), tail.len());
        abort()
    }
}

#[inline]
fn check(what: &'static str) {
    // `try_with` so allocations during thread teardown do not panic.
    if SEALED.try_with(|s| s.get()).unwrap_or(false) {
        violation(what);
    }
}

unsafe impl GlobalAlloc for Guard {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        check("alloc");
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        check("free");
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        check("realloc");
        System.realloc(p, l, n)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        check("alloc");
        System.alloc_zeroed(l)
    }
}

/// Run `f` with allocation forbidden on this thread. The result must not need
/// dropping heap memory inside the sealed region, so return plain values.
pub fn sealed<R>(f: impl FnOnce() -> R) -> R {
    SEALED.with(|s| s.set(true));
    let r = f();
    SEALED.with(|s| s.set(false));
    r
}

/// Run `f` with allocation permitted on this thread, even inside [`sealed`].
///
/// This is the one sanctioned exception to "no allocation after
/// initialization": establishing a TLS connection, where the handshake
/// library allocates. Nothing on the data path may use it.
pub fn allow_alloc<R>(f: impl FnOnce() -> R) -> R {
    let was = SEALED.with(|s| s.replace(false));
    let r = f();
    SEALED.with(|s| s.set(was));
    r
}
