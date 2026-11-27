// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Memory the backend maps itself and unmaps when it is dropped, never
//! handing it to an allocator.
//!
//! The kernel lets go of what a ring registered only in the ring's exit
//! work, after the ring's descriptor is closed: a card may still write
//! into a receive area then, and the kernel still reads a ring of returns.
//! Memory given back to an allocator could be handed out again while that
//! happens. Unmapped memory cannot: the kernel's pins keep its pages until
//! it lets go of them, and whatever is mapped at the same address later is
//! given pages of its own.

use super::*;
use core::ffi::c_void;
use core::ops::{Deref, DerefMut};

/// Anonymous memory, zeroed, of a length fixed when it is mapped.
pub(super) struct Mapped {
    ptr: *mut u8,
    len: usize,
}

impl Mapped {
    /// `len` bytes; with `populate`, their pages are faulted in now.
    pub(super) fn new(len: usize, populate: bool) -> io::Result<Mapped> {
        if len == 0 {
            return Ok(Mapped::default());
        }
        let flags = MAP_PRIVATE | MAP_ANONYMOUS | if populate { MAP_POPULATE } else { 0 };
        let ptr = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                flags,
                -1,
                0,
            )
        };
        if ptr == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapped {
            ptr: ptr.cast(),
            len,
        })
    }
}

impl Default for Mapped {
    /// Nothing mapped.
    fn default() -> Mapped {
        Mapped {
            ptr: core::ptr::NonNull::dangling().as_ptr(),
            len: 0,
        }
    }
}

impl Deref for Mapped {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl DerefMut for Mapped {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if self.len > 0 {
            let _: i32 = unsafe { munmap(self.ptr.cast::<c_void>(), self.len) };
        }
    }
}
