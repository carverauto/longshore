// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The kernels this build contains, and the choice between them.
//!
//! Public for the tests and the benchmark, which call each kernel directly.
//! Not part of the crate's interface.

#[cfg(target_arch = "aarch64")]
use crate::aarch64 as arch;
#[cfg(target_arch = "x86_64")]
use crate::x86_64 as arch;

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
mod arch {
    pub(crate) static KERNELS: [super::Kernel; 1] = [super::Kernel::TABLE];
    pub(crate) fn pick() -> usize {
        0
    }
}

/// One way of computing both checksums.
pub struct Kernel {
    pub name: &'static str,
    supported: fn() -> bool,
    crc64: unsafe fn(u64, &[u8]) -> u64,
    crc32c: unsafe fn(u32, &[u8]) -> u32,
}

impl Kernel {
    pub(crate) const TABLE: Kernel =
        Kernel::new("table", || true, crate::table::crc64, crate::table::crc32c);

    pub(crate) const fn new(
        name: &'static str,
        supported: fn() -> bool,
        crc64: unsafe fn(u64, &[u8]) -> u64,
        crc32c: unsafe fn(u32, &[u8]) -> u32,
    ) -> Kernel {
        Kernel {
            name,
            supported,
            crc64,
            crc32c,
        }
    }

    /// Whether this processor has the instructions the kernel uses.
    pub fn supported(&self) -> bool {
        (self.supported)()
    }

    /// Advances a CRC-64/NVME state. Panics on a processor without the
    /// kernel's instructions.
    pub fn crc64_update(&self, state: u64, data: &[u8]) -> u64 {
        assert!(self.supported(), "{} is not supported here", self.name);
        // SAFETY: the processor has the instructions, checked above.
        unsafe { (self.crc64)(state, data) }
    }

    /// Advances a CRC-32C state. Panics on a processor without the kernel's
    /// instructions.
    pub fn crc32c_update(&self, state: u32, data: &[u8]) -> u32 {
        assert!(self.supported(), "{} is not supported here", self.name);
        // SAFETY: the processor has the instructions, checked above.
        unsafe { (self.crc32c)(state, data) }
    }
}

/// Every kernel in this build, supported here or not.
pub fn all() -> &'static [Kernel] {
    &arch::KERNELS
}

/// The kernel the crate uses on this processor.
pub fn selected() -> &'static Kernel {
    &arch::KERNELS[arch::pick()]
}

#[inline]
pub(crate) fn crc64(state: u64, data: &[u8]) -> u64 {
    let kernel = selected();
    // SAFETY: `pick` returns only a kernel this processor supports.
    unsafe { (kernel.crc64)(state, data) }
}

#[inline]
pub(crate) fn crc32c(state: u32, data: &[u8]) -> u32 {
    let kernel = selected();
    // SAFETY: `pick` returns only a kernel this processor supports.
    unsafe { (kernel.crc32c)(state, data) }
}
