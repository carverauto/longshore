// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Writes for [`SimDisk`], of one run of the pool or of the spans of a
//! gathered write: either is one write, which meets one fault, placed or
//! drawn, and is synced once for a file opened with `dsync`.

use super::faults::{misdirect_slot_count, pick_misdirected_off, sector_cut_bounds};
use super::{DiskFaultCtx, PlacedFault, SimDisk};
use crate::io::count_result;
use crate::io::files::span_at;
use crate::io::posix::errno::linux::{EBADF, EIO, EISDIR, ENOSPC};
use crate::io::{FileSlot, Limits};

/// The bytes a write takes, in the order they go to the file.
pub(super) trait Bytes {
    /// How many there are.
    fn total(&self) -> usize;
    /// Writes the first `n` of them to inode `ii`, from `file_off` on.
    fn commit(&self, disk: &mut SimDisk, ii: usize, file_off: u64, n: usize);
}

impl Bytes for [u8] {
    fn total(&self) -> usize {
        self.len()
    }

    fn commit(&self, disk: &mut SimDisk, ii: usize, file_off: u64, n: usize) {
        disk.commit_write(ii, file_off, &self[..n]);
    }
}

/// The spans of a gathered write: `count` entries of the list at `at` of
/// the pool, read when the write completes.
pub(super) struct Spans<'a> {
    pub(super) pool: &'a [u8],
    pub(super) limits: &'a Limits,
    pub(super) at: usize,
    pub(super) count: u32,
}

impl Spans<'_> {
    /// The run of the pool that span `i` is.
    fn span(&self, i: u32) -> core::ops::Range<usize> {
        let (buf, off, len) = span_at(self.pool, self.at, i);
        self.limits.range(buf, off, len)
    }
}

impl Bytes for Spans<'_> {
    fn total(&self) -> usize {
        (0..self.count).map(|i| self.span(i).len()).sum()
    }

    fn commit(&self, disk: &mut SimDisk, ii: usize, file_off: u64, n: usize) {
        let (mut to, mut left) = (file_off, n);
        for i in 0..self.count {
            if left == 0 {
                return;
            }
            let r = self.span(i);
            let take = r.len().min(left);
            disk.commit_write(ii, to, &self.pool[r.start..r.start + take]);
            (to, left) = (to + take as u64, left - take);
        }
    }
}

impl SimDisk {
    pub(super) fn write(
        &mut self,
        slot: FileSlot,
        data: &(impl Bytes + ?Sized),
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
        let Some(end) = file_off.checked_add(data.total() as u64) else {
            return -ENOSPC;
        };
        if end > self.max_bytes_per_file as u64 {
            return -ENOSPC;
        }
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
        data: &(impl Bytes + ?Sized),
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let len = data.total();
        if len == 0 {
            return 0;
        }
        let s = u64::from(self.sector_size);
        if ctx.rng.chance(ctx.faults.lost_reported) {
            ctx.counters.lost_reported += 1;
            return -EIO;
        }
        if ctx.rng.chance(ctx.faults.lost_silent) {
            ctx.counters.lost_silent += 1;
            return count_result(len);
        }
        let cuts = sector_cut_bounds(file_off, len, s);
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
        let (other, ov_start, ov_count) = misdirect_slot_count(live, file_off, len, s);
        if other > 0 && ctx.rng.chance(ctx.faults.misdirected_write) {
            let dst = pick_misdirected_off(ctx.rng, other, ov_start, ov_count, s);
            data.commit(self, ii, dst, len);
            ctx.counters.misdirected_write += 1;
            return count_result(len);
        }
        data.commit(self, ii, file_off, len);
        count_result(len)
    }

    /// A write that meets the fault placed on its submission.
    fn write_placed(
        &mut self,
        ii: usize,
        file_off: u64,
        data: &(impl Bytes + ?Sized),
        pf: PlacedFault,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let len = data.total();
        let s = u64::from(self.sector_size);
        match pf {
            PlacedFault::Fail => {
                ctx.counters.placed_fail += 1;
                -EIO
            }
            PlacedFault::FailAfter => {
                data.commit(self, ii, file_off, len);
                ctx.counters.placed_fail_after += 1;
                -EIO
            }
            PlacedFault::LostReported => {
                ctx.counters.lost_reported += 1;
                -EIO
            }
            PlacedFault::LostSilent => {
                ctx.counters.lost_silent += 1;
                count_result(len)
            }
            PlacedFault::ShortWrite => {
                if sector_cut_bounds(file_off, len, s).is_some() {
                    self.inject_short_write(ii, file_off, data, ctx)
                } else {
                    data.commit(self, ii, file_off, len);
                    count_result(len)
                }
            }
            PlacedFault::TornReported => self.inject_torn_write(ii, file_off, data, true, ctx),
            PlacedFault::TornSilent => self.inject_torn_write(ii, file_off, data, false, ctx),
            PlacedFault::MisdirectedWrite => {
                let live = self.live_size(ii);
                let (other, ov_start, ov_count) = misdirect_slot_count(live, file_off, len, s);
                if other > 0 {
                    let dst = pick_misdirected_off(ctx.rng, other, ov_start, ov_count, s);
                    data.commit(self, ii, dst, len);
                    ctx.counters.misdirected_write += 1;
                } else {
                    data.commit(self, ii, file_off, len);
                }
                count_result(len)
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
}
