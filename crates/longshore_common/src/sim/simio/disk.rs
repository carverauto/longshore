// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Preallocated deterministic virtual disk for [`super::Virtual`].

use crate::io::files::STATX_SIZE;
use crate::io::posix::errno::linux::{EBADF, EINVAL};
use crate::io::{Limits, Op, OpenFlags};

#[path = "disk_ops.rs"]
mod ops;
#[path = "disk_paths.rs"]
mod paths;

/// Preallocated capacities for [`super::SimIo::setup_disk`].
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
}

impl Default for DiskConfig {
    fn default() -> Self {
        Self::BASE
    }
}

#[derive(Clone, Copy)]
pub(super) struct SimInode {
    pub(super) used: bool,
    pub(super) is_dir: bool,
    pub(super) data_idx: u32,
    pub(super) size: u64,
    pub(super) links: u32,
    pub(super) opens: u32,
    pub(super) ino: u64,
}

impl SimInode {
    const EMPTY: Self = Self {
        used: false,
        is_dir: false,
        data_idx: 0,
        size: 0,
        links: 0,
        opens: 0,
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
    const EMPTY: Self = Self {
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
    pub(super) max_name_len: usize,
    pub(super) inodes: Box<[SimInode]>,
    pub(super) data_used: Box<[bool]>,
    pub(super) file_data: Box<[u8]>,
    pub(super) names: Box<[SimName]>,
    pub(super) name_bytes: Box<[u8]>,
    pub(super) open_slots: Box<[Option<SimOpenFile>]>,
    pub(super) scratch_a: Box<[u8]>,
    pub(super) scratch_b: Box<[u8]>,
    pub(super) next_ino: u64,
}

impl SimDisk {
    pub(in crate::sim::simio) fn new(cfg: DiskConfig, files_max: u32) -> Self {
        let total_inodes = cfg.max_files as usize + cfg.max_dirs as usize + 1;
        let max_files = cfg.max_files as usize;
        let mut disk = Self {
            max_bytes_per_file: cfg.max_bytes_per_file,
            max_name_len: cfg.max_name_len,
            inodes: vec![SimInode::EMPTY; total_inodes].into_boxed_slice(),
            data_used: vec![false; max_files].into_boxed_slice(),
            file_data: vec![0u8; max_files * cfg.max_bytes_per_file].into_boxed_slice(),
            names: vec![SimName::EMPTY; total_inodes].into_boxed_slice(),
            name_bytes: vec![0u8; total_inodes * cfg.max_name_len].into_boxed_slice(),
            open_slots: vec![None; files_max as usize].into_boxed_slice(),
            scratch_a: vec![0u8; cfg.max_name_len].into_boxed_slice(),
            scratch_b: vec![0u8; cfg.max_name_len].into_boxed_slice(),
            next_ino: 1,
        };
        if total_inodes > 0 && cfg.max_name_len > 0 {
            disk.inodes[0] = SimInode {
                used: true,
                is_dir: true,
                data_idx: 0,
                size: 0,
                links: 1,
                opens: 0,
                ino: 1,
            };
            disk.names[0] = SimName {
                used: true,
                len: 1,
                inode: 0,
            };
            disk.name_bytes[0] = b'/';
            disk.next_ino = 2;
        }
        disk
    }

    pub(in crate::sim::simio) fn attempt(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
    ) -> i32 {
        match *op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                self.open(slot, &pool[r], flags)
            }
            Op::FileClose { slot } => self.close(slot),
            Op::FileRead {
                slot,
                buf,
                off,
                len,
                file_off,
            } => self.read(slot, pool, limits.range(buf, off, len), file_off),
            Op::FileWrite {
                slot,
                buf,
                off,
                len,
                file_off,
            } => self.write(slot, pool, limits.range(buf, off, len), file_off),
            Op::FileSync { slot, .. } => {
                if self
                    .open_slots
                    .get(slot.0 as usize)
                    .and_then(|s| *s)
                    .is_some()
                {
                    0
                } else {
                    -EBADF
                }
            }
            Op::FileReserve {
                slot,
                file_off,
                len,
                keep_size,
            } => self.reserve(slot, file_off, len, keep_size),
            Op::FilePunch {
                slot,
                file_off,
                len,
            } => self.punch(slot, file_off, len),
            Op::FileStatus {
                path_buf,
                path_off,
                path_len,
                out_buf,
                out_off,
            } => {
                let pr = limits.range(path_buf, path_off, path_len);
                let or = limits.range(out_buf, out_off, STATX_SIZE as u32);
                self.status(pool, pr, or)
            }
            Op::FileRename {
                old_buf,
                old_off,
                old_len,
                new_buf,
                new_off,
                new_len,
            } => {
                let r1 = limits.range(old_buf, old_off, old_len);
                let r2 = limits.range(new_buf, new_off, new_len);
                self.rename(pool, r1, r2)
            }
            Op::FileUnlink {
                path_buf,
                path_off,
                path_len,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                self.unlink(&pool[r])
            }
            Op::FileMkdir {
                path_buf,
                path_off,
                path_len,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                self.mkdir(&pool[r])
            }
            _ => -EINVAL,
        }
    }
}
