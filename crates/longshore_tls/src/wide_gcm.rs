// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! AWS-LC's 8-way AES-GCM on the Linux cores it is measured to be faster on
//! but does not recognise by itself.
//!
//! AWS-LC has two AES-GCM routines for AArch64: a 4-way one and an 8-way one
//! that also uses the SHA-3 `EOR3` instruction. It takes the 8-way routine
//! only on cores it recognises: Neoverse V1, V2 and V3 (Graviton 3 and 4)
//! by their part number on Linux, and Apple M cores only under macOS. Two
//! cores with the instructions and a measured gain are left out on Linux:
//!
//! - Apple M under a Linux virtual machine: +14% HTTPS upload, +11% download
//!   (bench/results/m28-tls-wide-gcm.md).
//! - Arm's Cortex-X925, in the DGX Spark: 12.2 against 7.8 GiB/s sealing
//!   (bench/results/m11-tls-fast-aes-aarch64-dgx-spark.md).
//!
//! [`enable`] reads the core's Main ID Register as AWS-LC does and, for those
//! two, when AWS-LC has found AES, PMULL and SHA3, sets the capability bit it
//! sets for Apple M cores. Besides the AES-GCM choice, that bit only selects
//! between two big-number multiplication routines that run on any AArch64
//! core (`bn/montgomery.c`, `s2n-bignum`). Other cores keep AWS-LC's own
//! choice: on them the 8-way routine is unmeasured, and whether it helps
//! depends on how many AES units the core has.

/// Apple's implementer code in the Main ID Register.
const IMPLEMENTER_APPLE: u64 = 0x61;
/// Arm's implementer code in the Main ID Register.
const IMPLEMENTER_ARM: u64 = 0x41;
/// Arm's part number for the Cortex-X925.
const PART_CORTEX_X925: u64 = 0xD85;

/// Whether a core with this Main ID Register value is one [`enable`] gives
/// the 8-way routine: any Apple core, or Arm's Cortex-X925.
#[must_use]
pub const fn listed(midr: u64) -> bool {
    let implementer = (midr >> 24) & 0xff;
    let part = (midr >> 4) & 0xfff;
    implementer == IMPLEMENTER_APPLE || (implementer == IMPLEMENTER_ARM && part == PART_CORTEX_X925)
}

/// The Main ID Register of the core this runs on, where Linux lets user
/// space read it; `None` elsewhere.
#[must_use]
pub fn midr() -> Option<u64> {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        linux::midr()
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    {
        None
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
mod linux {
    use core::ffi::c_ulong;

    extern "C" {
        fn getauxval(kind: c_ulong) -> c_ulong;
    }

    // linux/auxvec.h and asm/hwcap.h
    const AT_HWCAP: c_ulong = 16;
    const HWCAP_CPUID: c_ulong = 1 << 11;

    pub(super) fn midr() -> Option<u64> {
        // SAFETY: getauxval only reads the process's auxiliary vector.
        if unsafe { getauxval(AT_HWCAP) } & HWCAP_CPUID == 0 {
            return None;
        }
        let midr: u64;
        // SAFETY: with HWCAP_CPUID the kernel emulates this read for user
        // space; it touches no memory and no flags.
        unsafe {
            core::arch::asm!("mrs {}, MIDR_EL1", out(reg) midr, options(nomem, nostack, preserves_flags));
        }
        Some(midr)
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Once;

    // AWS-LC's capability word (crypto/fipsmodule/cpucap/internal.h). The
    // symbol carries the aws-lc-sys version, so a version change fails to
    // link rather than writing to a word whose meaning may have moved.
    extern "C" {
        #[link_name = "aws_lc_0_47_0_OPENSSL_armcap_P"]
        static CAPABILITIES: AtomicU32;
    }

    // include/openssl/arm_arch.h
    const ARMV8_AES: u32 = 1 << 2;
    const ARMV8_PMULL: u32 = 1 << 5;
    const ARMV8_SHA3: u32 = 1 << 11;
    const ARMV8_NEOVERSE_V1: u32 = 1 << 12;
    const ARMV8_APPLE_M: u32 = 1 << 13;
    const ARMV8_NEOVERSE_V2: u32 = 1 << 14;
    const ARMV8_NEOVERSE_V3: u32 = 1 << 18;
    const NEEDED: u32 = ARMV8_AES | ARMV8_PMULL | ARMV8_SHA3;
    const WIDE: u32 = ARMV8_NEOVERSE_V1 | ARMV8_APPLE_M | ARMV8_NEOVERSE_V2 | ARMV8_NEOVERSE_V3;

    static ONCE: Once = Once::new();

    fn capabilities() -> &'static AtomicU32 {
        // The library constructor has normally filled the word already;
        // this makes sure. SAFETY: AWS-LC's own one-time initialisation,
        // safe to call from any thread at any time.
        unsafe { aws_lc_sys::CRYPTO_library_init() };
        // SAFETY: a 32-bit word AWS-LC defines and keeps for the life of
        // the process. AtomicU32 has the layout of u32; it is written once,
        // below, with an atomic read-modify-write.
        unsafe { &CAPABILITIES }
    }

    pub(super) fn enable() -> bool {
        ONCE.call_once(|| {
            let caps = capabilities();
            let listed = super::midr().is_some_and(super::listed);
            if listed && caps.load(Ordering::Relaxed) & NEEDED == NEEDED {
                let _: u32 = caps.fetch_or(ARMV8_APPLE_M, Ordering::Relaxed);
            }
        });
        active()
    }

    pub(super) fn active() -> bool {
        let caps = capabilities().load(Ordering::Relaxed);
        caps & ARMV8_SHA3 != 0 && caps & WIDE != 0
    }
}

/// Lets AWS-LC use its 8-way AES-GCM routine on the listed cores
/// ([`listed`]) when it has found the instructions for it, and says whether
/// the routine is in use. Called before the first key is set; later calls
/// only report. On other architectures it does nothing and returns `false`.
#[inline]
pub fn enable() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        imp::enable()
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

/// Whether AWS-LC seals and opens with its 8-way AES-GCM routine.
#[inline]
pub fn active() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        imp::active()
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}
