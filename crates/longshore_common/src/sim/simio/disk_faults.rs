// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Disk fault helpers: sector boundary arithmetic, misdirected-write offsets,
//! persistent sector read errors, and placed-fault lookup for [`SimDisk`].

use super::{DiskFaultCtx, PlacedEntry, PlacedFault, Prng, SimDisk};
use crate::io::count_result;
use crate::io::posix::errno::linux::EIO;

#[derive(Clone, Copy)]
pub(super) struct ReadErrorSector {
    pub(super) used: bool,
    pub(super) ino: u64,
    pub(super) sector: u64,
}

impl ReadErrorSector {
    pub(super) const EMPTY: Self = Self {
        used: false,
        ino: 0,
        sector: 0,
    };
}

pub(super) fn take_placed(placed: &mut [PlacedEntry], op_num: u64) -> Option<PlacedFault> {
    if op_num == 0 {
        return None;
    }
    let mut found = None;
    for entry in placed.iter_mut() {
        if !entry.used && entry.op_num == op_num {
            entry.used = true;
            found = Some(entry.fault);
        }
    }
    found
}

pub(super) fn sector_cut_bounds(file_off: u64, len: usize, sector: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let end = file_off.saturating_add(len as u64);
    let first = (file_off / sector + 1) * sector;
    if first >= end {
        return None;
    }
    let last = ((end - 1) / sector) * sector;
    let num_cuts = (last - first) / sector + 1;
    Some((first, num_cuts))
}

pub(super) fn misdirect_slot_count(
    live_size: u64,
    file_off: u64,
    len: usize,
    sector: u64,
) -> (u64, u64, u64) {
    if len == 0 || (len as u64) > live_size {
        return (0, 0, 0);
    }
    let len_u = len as u64;
    let max_k = (live_size - len_u) / sector;
    let total = max_k + 1;
    let ov_lo = file_off.saturating_sub(len_u - 1).div_ceil(sector);
    let ov_hi = (file_off + len_u - 1) / sector;
    let ov_start = ov_lo.min(total);
    let ov_end = (ov_hi + 1).min(total);
    let ov_count = ov_end.saturating_sub(ov_start);
    (total - ov_count, ov_start, ov_count)
}

pub(super) fn pick_misdirected_off(
    rng: &mut Prng,
    other_slots: u64,
    ov_start: u64,
    ov_count: u64,
    sector: u64,
) -> u64 {
    let idx = rng.below(other_slots);
    let k = if idx < ov_start { idx } else { idx + ov_count };
    k * sector
}

impl SimDisk {
    pub(super) fn has_read_error(&self, ino: u64, first_sec: u64, last_sec: u64) -> bool {
        self.read_errors
            .iter()
            .any(|e| e.used && e.ino == ino && e.sector >= first_sec && e.sector <= last_sec)
    }

    pub(super) fn record_read_error(&mut self, ino: u64, sector: u64) {
        if self
            .read_errors
            .iter()
            .any(|e| e.used && e.ino == ino && e.sector == sector)
        {
            return;
        }
        if let Some(slot) = self.read_errors.iter_mut().find(|e| !e.used) {
            *slot = ReadErrorSector {
                used: true,
                ino,
                sector,
            };
        }
    }

    pub(super) fn clear_read_errors_range(&mut self, ino: u64, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let s = u64::from(self.sector_size);
        for entry in self.read_errors.iter_mut() {
            if entry.used && entry.ino == ino {
                let sec_start = entry.sector * s;
                let sec_end = sec_start + s;
                if start <= sec_start && sec_end <= end {
                    entry.used = false;
                }
            }
        }
    }

    pub(super) fn clear_read_errors_for_inode(&mut self, ino: u64) {
        for entry in self.read_errors.iter_mut() {
            if entry.used && entry.ino == ino {
                entry.used = false;
            }
        }
    }

    pub(super) fn inject_short_write(
        &mut self,
        ii: usize,
        file_off: u64,
        data: &[u8],
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let s = u64::from(self.sector_size);
        let cut_len = if let Some((first, num_cuts)) = sector_cut_bounds(file_off, data.len(), s) {
            (first + ctx.rng.below(num_cuts) * s - file_off) as usize
        } else {
            0
        };
        if cut_len > 0 {
            self.commit_write(ii, file_off, &data[..cut_len]);
        }
        ctx.counters.short_write += 1;
        count_result(cut_len)
    }

    pub(super) fn inject_torn_write(
        &mut self,
        ii: usize,
        file_off: u64,
        data: &[u8],
        reported: bool,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let s = u64::from(self.sector_size);
        if let Some((first, num_cuts)) = sector_cut_bounds(file_off, data.len(), s) {
            let cut_len = (first + ctx.rng.below(num_cuts) * s - file_off) as usize;
            if cut_len > 0 {
                self.commit_write(ii, file_off, &data[..cut_len]);
            }
        }
        if reported {
            ctx.counters.torn_reported += 1;
            -EIO
        } else {
            ctx.counters.torn_silent += 1;
            count_result(data.len())
        }
    }
}
