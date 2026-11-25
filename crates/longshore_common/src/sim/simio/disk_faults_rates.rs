// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters and `with_*` setters for [`DiskFaults`].

use super::DiskFaults;

impl DiskFaults {
    /// No faults: what `DiskFaults::default()` gives, usable in `const` items.
    pub const NONE: Self = Self {
        short_write: 0,
        torn_reported: 0,
        torn_silent: 0,
        lost_reported: 0,
        lost_silent: 0,
        misdirected_write: 0,
        read_error: 0,
        bit_flip: 0,
        sync_fail: 0,
        reserve_refused: 0,
        punch_refused: 0,
    };

    /// A write writes fewer bytes than asked, cut at a sector boundary.
    /// Models a kernel or block-layer short `write(2)` / `pwrite(2)` return.
    #[inline]
    pub const fn short_write(self) -> u32 {
        self.short_write
    }

    /// This value with [`DiskFaults::short_write`] set to `short_write`.
    #[inline]
    pub const fn with_short_write(mut self, short_write: u32) -> Self {
        self.short_write = short_write;
        self
    }

    /// A write leaves only a sector-aligned prefix on disk and returns `-EIO`.
    /// Models a power loss, controller reset, or bus error mid-transfer.
    #[inline]
    pub const fn torn_reported(self) -> u32 {
        self.torn_reported
    }

    /// This value with [`DiskFaults::torn_reported`] set to `torn_reported`.
    #[inline]
    pub const fn with_torn_reported(mut self, torn_reported: u32) -> Self {
        self.torn_reported = torn_reported;
        self
    }

    /// A write leaves only a sector-aligned prefix on disk and reports success.
    /// Models a silent torn write from drive cache or firmware non-atomicity.
    #[inline]
    pub const fn torn_silent(self) -> u32 {
        self.torn_silent
    }

    /// This value with [`DiskFaults::torn_silent`] set to `torn_silent`.
    #[inline]
    pub const fn with_torn_silent(mut self, torn_silent: u32) -> Self {
        self.torn_silent = torn_silent;
        self
    }

    /// A write leaves nothing on disk and returns `-EIO`.
    /// Models a rejected or aborted block write at the device or driver.
    #[inline]
    pub const fn lost_reported(self) -> u32 {
        self.lost_reported
    }

    /// This value with [`DiskFaults::lost_reported`] set to `lost_reported`.
    #[inline]
    pub const fn with_lost_reported(mut self, lost_reported: u32) -> Self {
        self.lost_reported = lost_reported;
        self
    }

    /// A write leaves nothing on disk and reports full success.
    /// Models a phantom/dropped write lost in volatile write-back cache.
    #[inline]
    pub const fn lost_silent(self) -> u32 {
        self.lost_silent
    }

    /// This value with [`DiskFaults::lost_silent`] set to `lost_silent`.
    #[inline]
    pub const fn with_lost_silent(mut self, lost_silent: u32) -> Self {
        self.lost_silent = lost_silent;
        self
    }

    /// A write lands at another sector-aligned offset in the file and reports success.
    /// Models a misdirected write where firmware writes to the wrong LBA.
    #[inline]
    pub const fn misdirected_write(self) -> u32 {
        self.misdirected_write
    }

    /// This value with [`DiskFaults::misdirected_write`] set to `misdirected_write`.
    #[inline]
    pub const fn with_misdirected_write(mut self, misdirected_write: u32) -> Self {
        self.misdirected_write = misdirected_write;
        self
    }

    /// A read returns `-EIO` and marks the sector bad until overwritten or punched.
    /// Models an unrecoverable latent sector read error (UNC).
    #[inline]
    pub const fn read_error(self) -> u32 {
        self.read_error
    }

    /// This value with [`DiskFaults::read_error`] set to `read_error`.
    #[inline]
    pub const fn with_read_error(mut self, read_error: u32) -> Self {
        self.read_error = read_error;
        self
    }

    /// A read flips one bit in the caller's buffer without altering the medium.
    /// Models a transient DMA, bus, or controller bit flip during read transfer.
    #[inline]
    pub const fn bit_flip(self) -> u32 {
        self.bit_flip
    }

    /// This value with [`DiskFaults::bit_flip`] set to `bit_flip`.
    #[inline]
    pub const fn with_bit_flip(mut self, bit_flip: u32) -> Self {
        self.bit_flip = bit_flip;
        self
    }

    /// A file or directory sync returns `-EIO` and marks covered writes un-durable.
    /// Models a failed `fsync(2)` / `fdatasync(2)` where Linux clears dirty pages with `AS_EIO`.
    /// A write to a file opened with [`crate::io::OpenFlags::dsync`] carries its own
    /// sync and rolls this rate too.
    #[inline]
    pub const fn sync_fail(self) -> u32 {
        self.sync_fail
    }

    /// This value with [`DiskFaults::sync_fail`] set to `sync_fail`.
    #[inline]
    pub const fn with_sync_fail(mut self, sync_fail: u32) -> Self {
        self.sync_fail = sync_fail;
        self
    }

    /// A space reservation fails with `-ENOSPC`.
    /// Models filesystem out-of-space on `fallocate(2)` / `F_PREALLOCATE`.
    #[inline]
    pub const fn reserve_refused(self) -> u32 {
        self.reserve_refused
    }

    /// This value with [`DiskFaults::reserve_refused`] set to `reserve_refused`.
    #[inline]
    pub const fn with_reserve_refused(mut self, reserve_refused: u32) -> Self {
        self.reserve_refused = reserve_refused;
        self
    }

    /// A hole punch fails with `-EOPNOTSUPP`.
    /// Models a filesystem refusing `FALLOC_FL_PUNCH_HOLE` / `F_PUNCHHOLE`.
    #[inline]
    pub const fn punch_refused(self) -> u32 {
        self.punch_refused
    }

    /// This value with [`DiskFaults::punch_refused`] set to `punch_refused`.
    #[inline]
    pub const fn with_punch_refused(mut self, punch_refused: u32) -> Self {
        self.punch_refused = punch_refused;
        self
    }
}
