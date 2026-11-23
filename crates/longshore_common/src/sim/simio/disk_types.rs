// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Public configuration, fault rates, placed faults, and counters for [`super::SimDisk`].

use super::Prng;

/// Preallocated capacities for [`super::super::SimIo::setup_disk`].
#[derive(Clone, Copy, Debug)]
pub struct DiskConfig {
    pub max_files: u32,
    pub max_dirs: u32,
    pub max_bytes_per_file: usize,
    pub max_name_len: usize,
    pub write_log_entries: u32,
    pub write_log_bytes: usize,
    pub read_error_sectors: u32,
    pub sector_size: u32,
}

impl DiskConfig {
    /// Zeroed base configuration for struct-update construction.
    pub const BASE: Self = Self {
        max_files: 0,
        max_dirs: 0,
        max_bytes_per_file: 0,
        max_name_len: 0,
        write_log_entries: 0,
        write_log_bytes: 0,
        read_error_sectors: 0,
        sector_size: 512,
    };

    pub(super) fn validate(&self) {
        assert!(
            self.sector_size == 512 || self.sector_size == 4096,
            "DiskConfig::sector_size must be 512 or 4096"
        );
        let total_inodes = (self.max_files as usize)
            .checked_add(self.max_dirs as usize)
            .and_then(|n| n.checked_add(1));
        assert!(total_inodes.is_some(), "DiskConfig inode count overflow");
        assert!(
            (self.max_files as usize)
                .checked_mul(self.max_bytes_per_file)
                .is_some(),
            "DiskConfig file_data size overflow"
        );
        assert!(
            total_inodes
                .and_then(|n| n.checked_mul(self.max_name_len))
                .is_some(),
            "DiskConfig name_bytes size overflow"
        );
    }
}

impl Default for DiskConfig {
    fn default() -> Self {
        Self::BASE
    }
}

/// Fault probabilities for simulated disk operations, in parts per million.
///
/// Every rate defaults to `0`, which skips the PRNG (`Prng::chance`) so
/// existing traces do not change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskFaults {
    /// A write writes fewer bytes than asked, cut at a sector boundary.
    /// Models a kernel or block-layer short `write(2)` / `pwrite(2)` return.
    pub short_write: u32,
    /// A write leaves only a sector-aligned prefix on disk and returns `-EIO`.
    /// Models a power loss, controller reset, or bus error mid-transfer.
    pub torn_reported: u32,
    /// A write leaves only a sector-aligned prefix on disk and reports success.
    /// Models a silent torn write from drive cache or firmware non-atomicity.
    pub torn_silent: u32,
    /// A write leaves nothing on disk and returns `-EIO`.
    /// Models a rejected or aborted block write at the device or driver.
    pub lost_reported: u32,
    /// A write leaves nothing on disk and reports full success.
    /// Models a phantom/dropped write lost in volatile write-back cache.
    pub lost_silent: u32,
    /// A write lands at another sector-aligned offset in the file and reports success.
    /// Models a misdirected write where firmware writes to the wrong LBA.
    pub misdirected_write: u32,
    /// A read returns `-EIO` and marks the sector bad until overwritten or punched.
    /// Models an unrecoverable latent sector read error (UNC).
    pub read_error: u32,
    /// A read flips one bit in the caller's buffer without altering the medium.
    /// Models a transient DMA, bus, or controller bit flip during read transfer.
    pub bit_flip: u32,
    /// A file or directory sync returns `-EIO` and marks covered writes un-durable.
    /// Models a failed `fsync(2)` / `fdatasync(2)` where Linux clears dirty pages with `AS_EIO`.
    pub sync_fail: u32,
    /// A space reservation fails with `-ENOSPC`.
    /// Models filesystem out-of-space on `fallocate(2)` / `F_PREALLOCATE`.
    pub reserve_refused: u32,
    /// A hole punch fails with `-EOPNOTSUPP`.
    /// Models a filesystem refusing `FALLOC_FL_PUNCH_HOLE` / `F_PUNCHHOLE`.
    pub punch_refused: u32,
}

/// Fault placed on a specific file operation by its submission-time sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacedFault {
    /// Fail before taking effect, returning `-EIO`. Models an immediate device I/O error.
    Fail,
    /// Take effect normally, then return `-EIO`. Models a lost or errored completion after commit.
    FailAfter,
    /// Cut a write at a sector boundary and return the written count. Models a short `write(2)`.
    ShortWrite,
    /// Write a sector-aligned prefix and return `-EIO`. Models a reported torn write.
    TornReported,
    /// Write a sector-aligned prefix and return the requested length. Models a silent torn write.
    TornSilent,
    /// Write nothing and return `-EIO`. Models a reported lost write.
    LostReported,
    /// Write nothing and return the requested length. Models a silent lost write.
    LostSilent,
    /// Write to another sector-aligned offset in the file and return success. Models a misdirected write.
    MisdirectedWrite,
}

/// Zero-allocation counters of injected disk faults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskFaultCounters {
    pub short_write: u64,
    pub torn_reported: u64,
    pub torn_silent: u64,
    pub lost_reported: u64,
    pub lost_silent: u64,
    pub misdirected_write: u64,
    pub read_error: u64,
    pub bit_flip: u64,
    pub sync_fail: u64,
    pub reserve_refused: u64,
    pub punch_refused: u64,
    pub placed_fail: u64,
    pub placed_fail_after: u64,
}

#[derive(Clone, Copy)]
pub(in crate::sim::simio) struct PlacedEntry {
    pub(in crate::sim::simio) op_num: u64,
    pub(in crate::sim::simio) fault: PlacedFault,
    pub(in crate::sim::simio) used: bool,
}

pub(in crate::sim::simio) struct DiskFaultCtx<'a> {
    pub(in crate::sim::simio) faults: &'a DiskFaults,
    pub(in crate::sim::simio) placed: &'a mut [PlacedEntry],
    pub(in crate::sim::simio) counters: &'a mut DiskFaultCounters,
    pub(in crate::sim::simio) rng: &'a mut Prng,
}
