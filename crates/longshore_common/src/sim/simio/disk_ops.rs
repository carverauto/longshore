// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Read, write, sync, space, status, and directory operations for [`SimDisk`].

use super::faults::{misdirect_slot_count, pick_misdirected_off, sector_cut_bounds};
use super::log::LogKind;
use super::{DiskFaultCtx, PlacedFault, SimDisk};
use crate::io::count_result;
use crate::io::files::STATX_SIZE;
use crate::io::posix::errno::linux::{EBADF, EINVAL, EIO, EISDIR, ENOENT, ENOSPC, EOPNOTSUPP};
use crate::io::FileSlot;

impl SimDisk {
    pub(super) fn read(
        &mut self,
        slot: FileSlot,
        pool: &mut [u8],
        range: core::ops::Range<usize>,
        file_off: u64,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let Some(open) = self.open_slots.get(slot.0 as usize).and_then(|s| *s) else {
            return -EBADF;
        };
        let ii = open.inode as usize;
        if self.inodes[ii].is_dir {
            return -EISDIR;
        }
        if !open.flags.read && open.flags.write {
            return -EBADF;
        }
        let live = self.live_size(ii);
        let n = if file_off >= live || range.is_empty() {
            0
        } else {
            range.len().min((live - file_off) as usize)
        };
        if let Some(pf) = placed {
            if pf == PlacedFault::FailAfter {
                if n > 0 {
                    self.read_overlay(ii, &mut pool[range.start..range.start + n], file_off);
                }
                ctx.counters.placed_fail_after += 1;
                return -EIO;
            }
            ctx.counters.placed_fail += 1;
            return -EIO;
        }
        if n == 0 {
            return 0;
        }
        let s = u64::from(self.sector_size);
        let first_sec = file_off / s;
        let last_sec = (file_off + n as u64 - 1) / s;
        let ino_num = self.inodes[ii].ino;
        if self.has_read_error(ino_num, first_sec, last_sec) {
            return -EIO;
        }
        if ctx.rng.chance(ctx.faults.read_error) {
            let sec = first_sec + ctx.rng.below(last_sec - first_sec + 1);
            self.record_read_error(ino_num, sec);
            ctx.counters.read_error += 1;
            return -EIO;
        }
        self.read_overlay(ii, &mut pool[range.start..range.start + n], file_off);
        if ctx.rng.chance(ctx.faults.bit_flip) {
            let byte_idx = ctx.rng.below(n as u64) as usize;
            let bit = 1u8 << ctx.rng.below(8);
            pool[range.start + byte_idx] ^= bit;
            ctx.counters.bit_flip += 1;
        }
        count_result(n)
    }

    pub(super) fn write(
        &mut self,
        slot: FileSlot,
        pool: &[u8],
        range: core::ops::Range<usize>,
        file_off: u64,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let Some(open) = self.open_slots.get(slot.0 as usize).and_then(|s| *s) else {
            return -EBADF;
        };
        let ii = open.inode as usize;
        if self.inodes[ii].is_dir {
            return -EISDIR;
        }
        if !open.flags.write {
            return -EBADF;
        }
        let len = range.len() as u64;
        let Some(end) = file_off.checked_add(len) else {
            return -ENOSPC;
        };
        if end > self.max_bytes_per_file as u64 {
            return -ENOSPC;
        }
        let data = &pool[range];
        let res = match placed {
            Some(pf) => self.write_placed(ii, file_off, data, pf, ctx),
            None => self.write_rolled(ii, file_off, data, ctx),
        };
        if open.flags.dsync && res > 0 {
            return self.settle_dsync_write(ii, res, ctx);
        }
        res
    }

    /// A write with no placed fault: each fault rate is rolled in turn.
    fn write_rolled(
        &mut self,
        ii: usize,
        file_off: u64,
        data: &[u8],
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        if data.is_empty() {
            return 0;
        }
        let s = u64::from(self.sector_size);
        if ctx.rng.chance(ctx.faults.lost_reported) {
            ctx.counters.lost_reported += 1;
            return -EIO;
        }
        if ctx.rng.chance(ctx.faults.lost_silent) {
            ctx.counters.lost_silent += 1;
            return count_result(data.len());
        }
        let cuts = sector_cut_bounds(file_off, data.len(), s);
        if cuts.is_some() && ctx.rng.chance(ctx.faults.short_write) {
            return self.inject_short_write(ii, file_off, data, ctx);
        }
        if ctx.rng.chance(ctx.faults.torn_reported) {
            return self.inject_torn_write(ii, file_off, data, true, ctx);
        }
        if ctx.rng.chance(ctx.faults.torn_silent) {
            return self.inject_torn_write(ii, file_off, data, false, ctx);
        }
        let live = self.live_size(ii);
        let (other, ov_start, ov_count) = misdirect_slot_count(live, file_off, data.len(), s);
        if other > 0 && ctx.rng.chance(ctx.faults.misdirected_write) {
            let dst = pick_misdirected_off(ctx.rng, other, ov_start, ov_count, s);
            self.commit_write(ii, dst, data);
            ctx.counters.misdirected_write += 1;
            return count_result(data.len());
        }
        self.commit_write(ii, file_off, data);
        count_result(data.len())
    }

    /// A write that meets the fault placed on its submission.
    fn write_placed(
        &mut self,
        ii: usize,
        file_off: u64,
        data: &[u8],
        pf: PlacedFault,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        match pf {
            PlacedFault::Fail => {
                ctx.counters.placed_fail += 1;
                -EIO
            }
            PlacedFault::FailAfter => {
                self.commit_write(ii, file_off, data);
                ctx.counters.placed_fail_after += 1;
                -EIO
            }
            PlacedFault::LostReported => {
                ctx.counters.lost_reported += 1;
                -EIO
            }
            PlacedFault::LostSilent => {
                ctx.counters.lost_silent += 1;
                count_result(data.len())
            }
            PlacedFault::ShortWrite => {
                let s = u64::from(self.sector_size);
                if sector_cut_bounds(file_off, data.len(), s).is_some() {
                    self.inject_short_write(ii, file_off, data, ctx)
                } else {
                    self.commit_write(ii, file_off, data);
                    count_result(data.len())
                }
            }
            PlacedFault::TornReported => self.inject_torn_write(ii, file_off, data, true, ctx),
            PlacedFault::TornSilent => self.inject_torn_write(ii, file_off, data, false, ctx),
            PlacedFault::MisdirectedWrite => {
                let s = u64::from(self.sector_size);
                let live = self.live_size(ii);
                let (other, ov_start, ov_count) =
                    misdirect_slot_count(live, file_off, data.len(), s);
                if other > 0 {
                    let dst = pick_misdirected_off(ctx.rng, other, ov_start, ov_count, s);
                    self.commit_write(ii, dst, data);
                    ctx.counters.misdirected_write += 1;
                } else {
                    self.commit_write(ii, file_off, data);
                }
                count_result(data.len())
            }
        }
    }

    pub(super) fn commit_write(&mut self, ii: usize, file_off: u64, data: &[u8]) {
        if !data.is_empty() {
            self.push_write_log(ii, file_off, data);
            let ino_num = self.inodes[ii].ino;
            self.clear_read_errors_range(ino_num, file_off, file_off + data.len() as u64);
        }
    }

    pub(super) fn attempt_sync(
        &mut self,
        slot: FileSlot,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let Some(open) = self.open_slots.get(slot.0 as usize).and_then(|s| *s) else {
            return -EBADF;
        };
        let ii = open.inode as usize;
        if let Some(pf) = placed {
            return match pf {
                PlacedFault::FailAfter => {
                    self.sync_inode(ii);
                    ctx.counters.placed_fail_after += 1;
                    -EIO
                }
                PlacedFault::Fail
                | PlacedFault::ShortWrite
                | PlacedFault::TornReported
                | PlacedFault::TornSilent
                | PlacedFault::LostReported
                | PlacedFault::LostSilent
                | PlacedFault::MisdirectedWrite => {
                    ctx.counters.placed_fail += 1;
                    -EIO
                }
            };
        }
        if ctx.rng.chance(ctx.faults.sync_fail) {
            self.poison_inode(ii);
            ctx.counters.sync_fail += 1;
            return -EIO;
        }
        self.sync_inode(ii);
        0
    }

    fn sync_inode(&mut self, ii: usize) {
        if self.inodes[ii].is_dir {
            self.sync_dir(ii);
        } else {
            self.sync_file(ii);
        }
    }

    fn poison_inode(&mut self, ii: usize) {
        if self.inodes[ii].is_dir {
            self.poison_dir(ii);
        } else {
            self.poison_file(ii);
        }
    }

    pub(super) fn reserve(
        &mut self,
        slot: FileSlot,
        file_off: u64,
        len: u64,
        keep_size: bool,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let Some(open) = self.open_slots.get(slot.0 as usize).and_then(|s| *s) else {
            return -EBADF;
        };
        let ii = open.inode as usize;
        if self.inodes[ii].is_dir {
            return -EISDIR;
        }
        if !open.flags.write {
            return -EBADF;
        }
        if len == 0 {
            return -EINVAL;
        }
        let Some(end) = file_off.checked_add(len) else {
            return -ENOSPC;
        };
        if end > self.max_bytes_per_file as u64 {
            return -ENOSPC;
        }
        if let Some(pf) = placed {
            if pf == PlacedFault::FailAfter {
                if !keep_size && end > self.live_size(ii) {
                    self.push_log_entry(ii, LogKind::Extend { end }, &[]);
                }
                ctx.counters.placed_fail_after += 1;
                return -EIO;
            }
            ctx.counters.placed_fail += 1;
            return -EIO;
        }
        if ctx.rng.chance(ctx.faults.reserve_refused) {
            ctx.counters.reserve_refused += 1;
            return -ENOSPC;
        }
        if !keep_size && end > self.live_size(ii) {
            self.push_log_entry(ii, LogKind::Extend { end }, &[]);
        }
        0
    }

    pub(super) fn punch(
        &mut self,
        slot: FileSlot,
        file_off: u64,
        len: u64,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let Some(open) = self.open_slots.get(slot.0 as usize).and_then(|s| *s) else {
            return -EBADF;
        };
        let ii = open.inode as usize;
        if self.inodes[ii].is_dir {
            return -EISDIR;
        }
        if !open.flags.write {
            return -EBADF;
        }
        if len == 0 {
            return -EINVAL;
        }
        if let Some(pf) = placed {
            if pf == PlacedFault::FailAfter {
                self.apply_punch(ii, file_off, len);
                ctx.counters.placed_fail_after += 1;
                return -EIO;
            }
            ctx.counters.placed_fail += 1;
            return -EIO;
        }
        if ctx.rng.chance(ctx.faults.punch_refused) {
            ctx.counters.punch_refused += 1;
            return -EOPNOTSUPP;
        }
        self.apply_punch(ii, file_off, len);
        0
    }

    fn apply_punch(&mut self, ii: usize, file_off: u64, len: u64) {
        self.push_log_entry(ii, LogKind::Punch { file_off, len }, &[]);
        let ino_num = self.inodes[ii].ino;
        self.clear_read_errors_range(ino_num, file_off, file_off.saturating_add(len));
    }

    pub(super) fn status(
        &mut self,
        pool: &mut [u8],
        path_range: core::ops::Range<usize>,
        out_range: core::ops::Range<usize>,
    ) -> i32 {
        let len = match Self::normalize_into(&pool[path_range], &mut self.scratch_a) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(ni) = self.find_name(&self.scratch_a[..len]) else {
            return -ENOENT;
        };
        let ii = self.names[ni].inode as usize;
        let ino = self.inodes[ii];
        let size = if ino.is_dir { 0 } else { self.live_size(ii) };
        let mut raw = [0u8; STATX_SIZE];
        let mode: u16 = if ino.is_dir { 0o040755 } else { 0o100644 };
        raw[0..4].copy_from_slice(&0x07ffu32.to_le_bytes());
        raw[4..8].copy_from_slice(&self.sector_size.max(4096).to_le_bytes());
        raw[16..20].copy_from_slice(&ino.live_links.to_le_bytes());
        raw[28..30].copy_from_slice(&mode.to_le_bytes());
        raw[32..40].copy_from_slice(&ino.ino.to_le_bytes());
        raw[40..48].copy_from_slice(&size.to_le_bytes());
        raw[48..56].copy_from_slice(&size.div_ceil(512).to_le_bytes());
        pool[out_range].copy_from_slice(&raw);
        0
    }
}
