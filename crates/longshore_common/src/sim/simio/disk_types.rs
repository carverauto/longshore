// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Public configuration, fault rates, placed faults, and counters for [`super::SimDisk`].

use super::Prng;

/// Preallocated capacities for [`super::super::SimIo::setup_disk`].
#[derive(Clone, Copy, Debug)]
pub struct DiskConfig {
    pub(crate) max_files: u32,
    pub(crate) max_dirs: u32,
    pub(crate) max_bytes_per_file: usize,
    pub(crate) max_name_len: usize,
    pub(crate) write_log_entries: u32,
    pub(crate) write_log_bytes: usize,
    pub(crate) read_error_sectors: u32,
    pub(crate) sector_size: u32,
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
    pub(crate) short_write: u32,
    pub(crate) torn_reported: u32,
    pub(crate) torn_silent: u32,
    pub(crate) lost_reported: u32,
    pub(crate) lost_silent: u32,
    pub(crate) misdirected_write: u32,
    pub(crate) read_error: u32,
    pub(crate) bit_flip: u32,
    pub(crate) sync_fail: u32,
    pub(crate) reserve_refused: u32,
    pub(crate) punch_refused: u32,
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
    pub(crate) short_write: u64,
    pub(crate) torn_reported: u64,
    pub(crate) torn_silent: u64,
    pub(crate) lost_reported: u64,
    pub(crate) lost_silent: u64,
    pub(crate) misdirected_write: u64,
    pub(crate) read_error: u64,
    pub(crate) bit_flip: u64,
    pub(crate) sync_fail: u64,
    pub(crate) reserve_refused: u64,
    pub(crate) punch_refused: u64,
    pub(crate) placed_fail: u64,
    pub(crate) placed_fail_after: u64,
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
