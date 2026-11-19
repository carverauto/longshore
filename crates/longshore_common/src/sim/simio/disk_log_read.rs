// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Poisoned per-inode overlay helpers, write-log overlay reads, medium bit
//! flips, and crash write settlement for [`SimDisk`].

use super::faults::sector_cut_bounds;
use super::log::LogKind;
use super::{Prng, SimDisk};
use crate::io::posix::errno::linux::{EINVAL, EISDIR, ENOENT};

impl SimDisk {
    pub(super) fn poisoned_bit(&self, di: usize, off: usize) -> bool {
        let idx = di * self.mask_stride + (off >> 3);
        (self.poisoned_mask[idx] >> (off & 7)) & 1 != 0
    }

    fn set_poisoned_mask(&mut self, di: usize, start: usize, end: usize, val: bool) {
        if end <= start {
            return;
        }
        let base = di * self.mask_stride;
        for off in start..end {
            let slot = &mut self.poisoned_mask[base + (off >> 3)];
            let bit = 1u8 << (off & 7);
            if val {
                *slot |= bit;
            } else {
                *slot &= !bit;
            }
        }
    }

    fn set_poisoned_zero(&mut self, di: usize, start: usize, end: usize) {
        if end <= start {
            return;
        }
        let p_base = di * self.max_bytes_per_file;
        self.poisoned_data[p_base + start..p_base + end].fill(0);
        self.set_poisoned_mask(di, start, end, true);
    }

    pub(super) fn clear_poisoned_range(&mut self, di: usize, start: usize, end: usize) {
        self.set_poisoned_mask(di, start, end, false);
    }

    pub(super) fn clear_poisoned_inode(&mut self, di: usize) {
        if self.mask_stride > 0 {
            let base = di * self.mask_stride;
            self.poisoned_mask[base..base + self.mask_stride].fill(0);
        }
        if di < self.poisoned_eof.len() {
            self.poisoned_eof[di] = None;
        }
    }

    pub(super) fn fold_entry_to_poisoned(&mut self, di: usize, ii: usize, kind: LogKind) {
        let cur_sz = self.poisoned_eof[di].unwrap_or(self.inodes[ii].durable_size);
        match kind {
            LogKind::Write {
                file_off,
                len,
                buf_off,
            } => {
                let n = len as usize;
                if n > 0 {
                    if file_off > cur_sz {
                        self.set_poisoned_zero(di, cur_sz as usize, file_off as usize);
                    }
                    let p_base = di * self.max_bytes_per_file + file_off as usize;
                    self.poisoned_data[p_base..p_base + n]
                        .copy_from_slice(&self.log_bytes[buf_off..buf_off + n]);
                    self.set_poisoned_mask(di, file_off as usize, file_off as usize + n, true);
                    self.poisoned_eof[di] = Some(cur_sz.max(file_off + n as u64));
                }
            }
            LogKind::Punch { file_off, len } => {
                let s = file_off.min(cur_sz) as usize;
                let e = file_off.saturating_add(len).min(cur_sz) as usize;
                if e > s {
                    self.set_poisoned_zero(di, s, e);
                    self.poisoned_eof[di] = Some(cur_sz);
                }
            }
            LogKind::Truncate => {
                if cur_sz > 0 {
                    self.set_poisoned_zero(di, 0, cur_sz as usize);
                }
                self.poisoned_eof[di] = Some(0);
            }
            LogKind::Extend { end } => {
                if end > cur_sz {
                    self.set_poisoned_zero(di, cur_sz as usize, end as usize);
                    self.poisoned_eof[di] = Some(end);
                }
            }
            LogKind::Empty => {}
        }
    }

    pub(super) fn read_overlay(&self, ii: usize, dst: &mut [u8], file_off: u64) {
        let n = dst.len();
        let di = self.inodes[ii].data_idx as usize;
        let ds = self.inodes[ii].durable_size;
        let d_base = di * self.max_bytes_per_file;
        let durable_avail = ds.saturating_sub(file_off).min(n as u64) as usize;
        if durable_avail > 0 {
            let src = d_base + file_off as usize;
            dst[..durable_avail].copy_from_slice(&self.durable_data[src..src + durable_avail]);
        }
        if durable_avail < n {
            dst[durable_avail..].fill(0);
        }
        if self.poisoned_eof[di].is_some() {
            let p_base = di * self.max_bytes_per_file;
            let off = file_off as usize;
            let limit = self.max_bytes_per_file.saturating_sub(off).min(n);
            for (k, byte) in dst[..limit].iter_mut().enumerate() {
                if self.poisoned_bit(di, off + k) {
                    *byte = self.poisoned_data[p_base + off + k];
                }
            }
        }
        let read_end = file_off + n as u64;
        for entry in &self.log_entries[..self.log_len] {
            if entry.inode as usize != ii {
                continue;
            }
            match entry.kind {
                LogKind::Truncate => dst.fill(0),
                LogKind::Punch {
                    file_off: p_off,
                    len: p_len,
                } => {
                    let lo = file_off.max(p_off);
                    let hi = read_end.min(p_off.saturating_add(p_len));
                    if hi > lo {
                        let d0 = (lo - file_off) as usize;
                        let d1 = (hi - file_off) as usize;
                        dst[d0..d1].fill(0);
                    }
                }
                LogKind::Write {
                    file_off: w_off,
                    len: w_len,
                    buf_off,
                } => {
                    let lo = file_off.max(w_off);
                    let hi = read_end.min(w_off + w_len as u64);
                    if hi > lo {
                        let d0 = (lo - file_off) as usize;
                        let count = (hi - lo) as usize;
                        let s0 = buf_off + (lo - w_off) as usize;
                        dst[d0..d0 + count].copy_from_slice(&self.log_bytes[s0..s0 + count]);
                    }
                }
                LogKind::Extend { .. } | LogKind::Empty => {}
            }
        }
    }

    pub(in crate::sim::simio) fn corrupt_medium_bit(
        &mut self,
        raw_path: &[u8],
        file_off: u64,
        bit: u8,
    ) -> Result<(), i32> {
        assert!(bit < 8, "bit index must be < 8");
        let len = Self::normalize_into(raw_path, &mut self.scratch_a)?;
        let Some(ni) = self.find_name(&self.scratch_a[..len]) else {
            return Err(-ENOENT);
        };
        let ii = self.names[ni].inode as usize;
        if self.inodes[ii].is_dir {
            return Err(-EISDIR);
        }
        let max_sz = self.live_size(ii).max(self.inodes[ii].durable_size);
        if file_off >= max_sz {
            return Err(-EINVAL);
        }
        let mask = 1u8 << bit;
        let di = self.inodes[ii].data_idx as usize;
        if file_off < self.inodes[ii].durable_size {
            let d_idx = di * self.max_bytes_per_file + file_off as usize;
            self.durable_data[d_idx] ^= mask;
        }
        if self.poisoned_eof[di].is_some() && self.poisoned_bit(di, file_off as usize) {
            let p_idx = di * self.max_bytes_per_file + file_off as usize;
            self.poisoned_data[p_idx] ^= mask;
        }
        for entry in &self.log_entries[..self.log_len] {
            if entry.inode as usize != ii {
                continue;
            }
            if let LogKind::Write {
                file_off: w_off,
                len: w_len,
                buf_off,
            } = entry.kind
            {
                if file_off >= w_off && file_off < w_off + w_len as u64 {
                    self.log_bytes[buf_off + (file_off - w_off) as usize] ^= mask;
                }
            }
        }
        Ok(())
    }

    pub(super) fn settle_write_log(&mut self, rng: &mut Prng) {
        let s = self.sector_size as u64;
        for idx in 0..self.log_len {
            let entry = self.log_entries[idx];
            let ii = entry.inode as usize;
            match entry.kind {
                LogKind::Write {
                    file_off,
                    len,
                    buf_off,
                } => {
                    if len == 0 {
                        continue;
                    }
                    let keep_len = if let Some((first_b, num_cuts)) =
                        sector_cut_bounds(file_off, len as usize, s)
                    {
                        match rng.below(3) {
                            0 => 0,
                            1 => len as usize,
                            _ => (first_b + rng.below(num_cuts) * s - file_off) as usize,
                        }
                    } else if rng.below(2) == 1 {
                        len as usize
                    } else {
                        0
                    };
                    if keep_len > 0 {
                        self.apply_kind_to_durable(
                            ii,
                            LogKind::Write {
                                file_off,
                                len: keep_len as u32,
                                buf_off,
                            },
                        );
                    }
                }
                LogKind::Punch { .. } | LogKind::Truncate | LogKind::Extend { .. } => {
                    if rng.below(2) == 1 {
                        self.apply_kind_to_durable(ii, entry.kind);
                    }
                }
                LogKind::Empty => {}
            }
        }
        self.log_len = 0;
        self.log_bytes_used = 0;
        self.log_arena_tail = 0;
        self.poisoned_mask.fill(0);
        self.poisoned_eof.fill(None);
    }
}
