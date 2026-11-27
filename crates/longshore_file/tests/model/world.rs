// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The campaigns, and the set and simulated disk each run is on.

use longshore_common::io::Limits;
use longshore_common::sim::{DiskConfig, DiskFaults, Faults, SimIo};
use longshore_file::check::{self, Rig};
use longshore_file::FileSizes;

/// Bytes of an extent in the model: two blocks.
pub const EXTENT: u32 = 8192;
/// The block size.
pub const BLOCK: u32 = 4096;
/// The set's sizes: three files of three operations, three shared slots,
/// two path slots, sequences of three, four spans.
pub const FILES: u32 = 3;
pub const PER_FILE: u32 = 3;
pub const SHARED: u32 = 3;
pub const PATHS: u32 = 2;
pub const STEPS_MAX: u32 = 3;
pub const SPANS_MAX: u32 = 4;
/// Pages of the receive area, written from as spans.
pub const PAGES: u32 = 2;

/// What a run's disk does wrong.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Campaign {
    /// Nothing.
    Clean,
    /// Faults the disk reports: short, torn and lost writes, read errors,
    /// refused reserves and punches, and faults placed on single
    /// operations.
    Reported,
    /// Failed syncs (and durable writes), with some short writes.
    SyncFails,
    /// Torn and short writes, and power losses.
    Torn,
    /// Power losses on a disk that does nothing else wrong.
    PowerLoss,
    /// Silent faults and power losses: what bytes hold is not checked
    /// (C12), everything else is.
    Silent,
}

impl Campaign {
    /// Every campaign.
    pub const ALL: [Campaign; 6] = [
        Campaign::Clean,
        Campaign::Reported,
        Campaign::SyncFails,
        Campaign::Torn,
        Campaign::PowerLoss,
        Campaign::Silent,
    ];

    /// The campaign named `name`, as `Debug` prints it.
    pub fn named(name: &str) -> Option<Campaign> {
        Campaign::ALL
            .into_iter()
            .find(|c| format!("{c:?}").eq_ignore_ascii_case(name))
    }

    /// Its disk's fault rates, in parts per million.
    pub fn faults(self) -> DiskFaults {
        let none = DiskFaults::NONE;
        match self {
            Campaign::Clean | Campaign::PowerLoss => none,
            Campaign::Reported => none
                .with_short_write(50_000)
                .with_torn_reported(10_000)
                .with_lost_reported(10_000)
                .with_read_error(2_000)
                .with_reserve_refused(20_000)
                .with_punch_refused(20_000),
            Campaign::SyncFails => none.with_sync_fail(150_000).with_short_write(20_000),
            Campaign::Torn => none.with_torn_reported(40_000).with_short_write(40_000),
            Campaign::Silent => none
                .with_torn_silent(20_000)
                .with_lost_silent(20_000)
                .with_misdirected_write(20_000)
                .with_bit_flip(20_000),
        }
    }

    /// Faults placed on single operations, per million of them.
    pub const fn placed(self) -> u32 {
        match self {
            Campaign::Reported => 50_000,
            Campaign::SyncFails => 30_000,
            Campaign::Clean | Campaign::Torn | Campaign::PowerLoss | Campaign::Silent => 0,
        }
    }

    /// Power losses, per million steps.
    pub const fn power_losses(self) -> u32 {
        match self {
            Campaign::Torn | Campaign::PowerLoss | Campaign::Silent => 10_000,
            Campaign::Clean | Campaign::Reported | Campaign::SyncFails => 0,
        }
    }

    /// Whether the disk reports every fault: then a failed operation is
    /// one the campaign allows, and bytes are checked (F7, F8).
    pub const fn faulty(self) -> bool {
        !matches!(self, Campaign::Clean | Campaign::PowerLoss)
    }

    /// Whether bytes are checked: not under silent faults (C12).
    pub const fn checks_bytes(self) -> bool {
        !matches!(self, Campaign::Silent)
    }
}

/// The backend's limits before the plan: extents of two blocks, pages of
/// the receive area.
pub fn limits() -> Limits {
    check::limits()
        .with_aligned_buffers(16)
        .with_aligned_buffer_size(EXTENT)
        .with_area_pages(PAGES)
}

/// The set's sizes.
pub fn sizes() -> FileSizes {
    FileSizes::new(FILES, PER_FILE)
        .with_shared(SHARED)
        .with_paths(PATHS)
        .with_sequence_max(STEPS_MAX)
        .with_spans_max(SPANS_MAX)
}

/// A set on a simulated disk of 512- or 4096-byte sectors, by the seed.
pub fn rig(campaign: Campaign, seed: u64) -> Rig<SimIo> {
    let sector = if seed.is_multiple_of(3) { 4096 } else { 512 };
    let disk = DiskConfig::BASE
        .with_max_files(96)
        .with_max_dirs(32)
        .with_max_bytes_per_file(1 << 20)
        .with_max_name_len(64)
        .with_write_log_entries(512)
        .with_write_log_bytes(4 << 20)
        .with_read_error_sectors(64)
        .with_sector_size(sector);
    let mut make = |l: Limits| {
        let mut s = SimIo::new(l, seed, Faults::NONE);
        s.setup_disk(disk);
        s.set_disk_faults(campaign.faults());
        s
    };
    Rig::new(&mut make, limits(), sizes())
}
