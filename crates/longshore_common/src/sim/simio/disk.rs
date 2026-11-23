// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Preallocated deterministic virtual disk for [`super::Virtual`].
//!
//! Model rules are adapted from UltraGraphDB's
//! `ultragraphdb_transactions_log::SimStorage`, which followed Apache Iggy's
//! simulated storage, rewritten anew against `longshore_common::io::Io` with
//! zero heap allocation after [`super::SimIo::setup_disk`].
//!
//! Differences from `SimStorage`:
//!
//! | `SimStorage` | Here (`SimDisk`) | Why |
//! |---|---|---|
//! | Power loss reverts everything to stable | Each unsynced write settled by the seed | A real disk keeps some of them |
//! | A write of a sector or less can tear inside the sector | It is kept or dropped | A sector is written whole or not |
//! | A failed sync keeps the buffered data for a retry | The data is never made durable (`AS_EIO`) | Linux's behaviour |
//! | Short-torn write: an error, and a prefix | Short write: success and a count; torn reported: error and a prefix | The two cases a kernel can return |
//! | One rate, equal odds between modes | A rate per fault | Tests can isolate one fault |
//! | Truncate, list, length by handle | None; status by path | `add-file-operations` has none |
//! | A log of every operation | Counters per fault | Nothing allocates |
//! | Its own generator | The simulator's | One seed for network and disk |

use super::Prng;
use crate::io::OpenFlags;

#[path = "disk_attempt.rs"]
mod attempt;
#[path = "disk_dirs.rs"]
mod dirs;
#[path = "disk_dirs_apply.rs"]
mod dirs_apply;
#[path = "disk_faults.rs"]
mod faults;
#[path = "disk_log.rs"]
mod log;
#[path = "disk_log_read.rs"]
mod log_read;
#[path = "disk_ns.rs"]
mod ns;
#[path = "disk_ops.rs"]
mod ops;
#[path = "disk_paths.rs"]
mod paths;
#[path = "disk_types.rs"]
mod types;

pub use types::{DiskConfig, DiskFaultCounters, DiskFaults, PlacedFault};
pub(in crate::sim::simio) use types::{DiskFaultCtx, PlacedEntry};

use dirs::DirChange;
use faults::ReadErrorSector;
use log::WriteLogEntry;

#[derive(Clone, Copy)]
pub(super) struct SimInode {
    pub(super) used: bool,
    pub(super) is_dir: bool,
    pub(super) data_idx: u32,
    pub(super) durable_size: u64,
    pub(super) live_links: u32,
    pub(super) durable_links: u32,
    pub(super) opens: u32,
    pub(super) dir_log_refs: u32,
    pub(super) ino: u64,
}

impl SimInode {
    pub(super) const EMPTY: Self = Self {
        used: false,
        is_dir: false,
        data_idx: 0,
        durable_size: 0,
        live_links: 0,
        durable_links: 0,
        opens: 0,
        dir_log_refs: 0,
        ino: 0,
    };
}

#[derive(Clone, Copy)]
pub(super) struct SimName {
    pub(super) used: bool,
    pub(super) len: u16,
    pub(super) inode: u32,
}

impl SimName {
    pub(super) const EMPTY: Self = Self {
        used: false,
        len: 0,
        inode: 0,
    };
}

#[derive(Clone, Copy)]
pub(super) struct SimOpenFile {
    pub(super) inode: u32,
    pub(super) flags: OpenFlags,
}

pub(in crate::sim::simio) struct SimDisk {
    pub(super) max_bytes_per_file: usize,
    pub(super) mask_stride: usize,
    pub(super) max_name_len: usize,
    pub(super) sector_size: u32,
    pub(super) max_log_entries: usize,
    pub(super) inodes: Box<[SimInode]>,
    pub(super) data_used: Box<[bool]>,
    pub(super) durable_data: Box<[u8]>,
    pub(super) poisoned_data: Box<[u8]>,
    pub(super) poisoned_mask: Box<[u8]>,
    pub(super) poisoned_eof: Box<[Option<u64>]>,
    pub(super) names: Box<[SimName]>,
    pub(super) name_bytes: Box<[u8]>,
    pub(super) durable_names: Box<[SimName]>,
    pub(super) durable_name_bytes: Box<[u8]>,
    pub(super) open_slots: Box<[Option<SimOpenFile>]>,
    log_entries: Box<[WriteLogEntry]>,
    log_len: usize,
    log_bytes: Box<[u8]>,
    log_bytes_used: usize,
    log_arena_tail: usize,
    dir_log: Box<[DirChange]>,
    dir_log_len: usize,
    dir_arena_used: Box<[bool]>,
    dir_name_arena: Box<[u8]>,
    read_errors: Box<[ReadErrorSector]>,
    pub(super) scratch_a: Box<[u8]>,
    pub(super) scratch_b: Box<[u8]>,
    pub(super) durable_scratch_a: Box<[u8]>,
    pub(super) durable_scratch_b: Box<[u8]>,
    pub(super) next_ino: u64,
}

impl SimDisk {
    pub(in crate::sim::simio) fn new(cfg: DiskConfig, files_max: u32) -> Self {
        cfg.validate();
        let total_inodes = cfg.max_files as usize + cfg.max_dirs as usize + 1;
        let max_files = cfg.max_files as usize;
        let max_log_entries = cfg.write_log_entries as usize;
        let mask_stride = cfg.max_bytes_per_file.div_ceil(8);
        let dir_cap = (total_inodes * 4).max(64);
        let mut disk = Self {
            max_bytes_per_file: cfg.max_bytes_per_file,
            mask_stride,
            max_name_len: cfg.max_name_len,
            sector_size: cfg.sector_size,
            max_log_entries,
            inodes: vec![SimInode::EMPTY; total_inodes].into_boxed_slice(),
            data_used: vec![false; max_files].into_boxed_slice(),
            durable_data: vec![0u8; max_files * cfg.max_bytes_per_file].into_boxed_slice(),
            poisoned_data: vec![0u8; max_files * cfg.max_bytes_per_file].into_boxed_slice(),
            poisoned_mask: vec![0u8; max_files * mask_stride].into_boxed_slice(),
            poisoned_eof: vec![None; max_files].into_boxed_slice(),
            names: vec![SimName::EMPTY; total_inodes].into_boxed_slice(),
            name_bytes: vec![0u8; total_inodes * cfg.max_name_len].into_boxed_slice(),
            durable_names: vec![SimName::EMPTY; total_inodes].into_boxed_slice(),
            durable_name_bytes: vec![0u8; total_inodes * cfg.max_name_len].into_boxed_slice(),
            open_slots: vec![None; files_max as usize].into_boxed_slice(),
            log_entries: vec![WriteLogEntry::EMPTY; max_log_entries].into_boxed_slice(),
            log_len: 0,
            log_bytes: vec![0u8; cfg.write_log_bytes].into_boxed_slice(),
            log_bytes_used: 0,
            log_arena_tail: 0,
            dir_log: vec![DirChange::EMPTY; dir_cap].into_boxed_slice(),
            dir_log_len: 0,
            dir_arena_used: vec![false; dir_cap].into_boxed_slice(),
            dir_name_arena: vec![0u8; dir_cap * 2 * cfg.max_name_len].into_boxed_slice(),
            read_errors: vec![ReadErrorSector::EMPTY; cfg.read_error_sectors as usize]
                .into_boxed_slice(),
            scratch_a: vec![0u8; cfg.max_name_len].into_boxed_slice(),
            scratch_b: vec![0u8; cfg.max_name_len].into_boxed_slice(),
            durable_scratch_a: vec![0u8; cfg.max_name_len].into_boxed_slice(),
            durable_scratch_b: vec![0u8; cfg.max_name_len].into_boxed_slice(),
            next_ino: 1,
        };
        if total_inodes > 0 && cfg.max_name_len > 0 {
            disk.inodes[0] = SimInode {
                used: true,
                is_dir: true,
                data_idx: 0,
                durable_size: 0,
                live_links: 1,
                durable_links: 1,
                opens: 0,
                dir_log_refs: 0,
                ino: 1,
            };
            let root = SimName {
                used: true,
                len: 1,
                inode: 0,
            };
            disk.names[0] = root;
            disk.name_bytes[0] = b'/';
            disk.durable_names[0] = root;
            disk.durable_name_bytes[0] = b'/';
            disk.next_ino = 2;
        }
        disk
    }

    pub(in crate::sim::simio) fn power_loss(&mut self, rng: &mut Prng) {
        self.open_slots.fill(None);
        for ino in self.inodes.iter_mut() {
            ino.opens = 0;
        }
        self.settle_write_log(rng);
        self.settle_dir_log(rng);
        self.names.copy_from_slice(&self.durable_names);
        self.name_bytes.copy_from_slice(&self.durable_name_bytes);
        for ino in self.inodes.iter_mut() {
            ino.live_links = ino.durable_links;
        }
        for ii in 0..self.inodes.len() {
            self.free_inode_if_dead(ii);
        }
    }
}
