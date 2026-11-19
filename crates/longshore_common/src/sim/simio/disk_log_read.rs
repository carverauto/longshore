// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Write-log overlay reads, poisoned-write trimming, medium bit flips, and
//! crash write settlement for [`SimDisk`].

use super::faults::sector_cut_bounds;
use super::log::{LogKind, WriteLogEntry};
use super::{Prng, SimDisk};
use crate::io::posix::errno::linux::{EINVAL, EISDIR, ENOENT};

impl SimDisk {
    pub(super) fn trim_poisoned(&mut self, ii: usize, limit_idx: usize, kind: LogKind) {
        let (w_start, w_end, is_trunc) = match kind {
            LogKind::Write { file_off, len, .. } => (file_off, file_off + len as u64, false),
            LogKind::Punch { file_off, len } => (file_off, file_off.saturating_add(len), false),
            LogKind::Truncate => (0, 0, true),
            LogKind::Extend { .. } | LogKind::Empty => return,
        };
        if !is_trunc && w_start == w_end {
            return;
        }
        let mut p_idx = 0usize;
        let mut end = limit_idx.min(self.log_len);
        while p_idx < end {
            let p = self.log_entries[p_idx];
            if p.inode as usize != ii || !p.poisoned {
                p_idx += 1;
                continue;
            }
            if is_trunc {
                self.remove_log_entry(p_idx);
                end -= 1;
                continue;
            }
            match p.kind {
                LogKind::Write {
                    file_off: p_off,
                    len: p_len,
                    buf_off,
                } => {
                    let p_start = p_off;
                    let p_end = p_off + p_len as u64;
                    if w_end <= p_start || p_end <= w_start {
                        p_idx += 1;
                    } else if w_start <= p_start && p_end <= w_end {
                        self.remove_log_entry(p_idx);
                        end -= 1;
                    } else if w_start <= p_start {
                        let cut = (w_end - p_start) as usize;
                        self.log_entries[p_idx].kind = LogKind::Write {
                            file_off: w_end,
                            len: p_len - cut as u32,
                            buf_off: buf_off + cut,
                        };
                        self.log_bytes_used -= cut;
                        p_idx += 1;
                    } else if p_end <= w_end {
                        let cut = (p_end - w_start) as usize;
                        self.log_entries[p_idx].kind = LogKind::Write {
                            file_off: p_start,
                            len: p_len - cut as u32,
                            buf_off,
                        };
                        self.log_bytes_used -= cut;
                        p_idx += 1;
                    } else {
                        let left_len = (w_start - p_start) as usize;
                        let mid_len = (w_end - w_start) as usize;
                        let right_len = (p_end - w_end) as usize;
                        self.log_entries[p_idx].kind = LogKind::Write {
                            file_off: p_start,
                            len: left_len as u32,
                            buf_off,
                        };
                        self.log_bytes_used -= mid_len;
                        if self.log_len < self.log_entries.len() {
                            self.log_entries
                                .copy_within(p_idx + 1..self.log_len, p_idx + 2);
                            self.log_entries[p_idx + 1] = WriteLogEntry {
                                inode: ii as u32,
                                poisoned: true,
                                kind: LogKind::Write {
                                    file_off: w_end,
                                    len: right_len as u32,
                                    buf_off: buf_off + left_len + mid_len,
                                },
                            };
                            self.log_len += 1;
                            end += 1;
                            p_idx += 2;
                        } else {
                            p_idx += 1;
                        }
                    }
                }
                LogKind::Punch { file_off, len } => {
                    let p_end = file_off.saturating_add(len);
                    if w_start <= file_off && p_end <= w_end {
                        self.remove_log_entry(p_idx);
                        end -= 1;
                    } else {
                        p_idx += 1;
                    }
                }
                LogKind::Truncate | LogKind::Extend { .. } | LogKind::Empty => p_idx += 1,
            }
        }
    }

    pub(super) fn read_overlay(&self, ii: usize, dst: &mut [u8], file_off: u64) {
        let n = dst.len();
        let ds = self.inodes[ii].durable_size;
        let d_base = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
        let durable_avail = ds.saturating_sub(file_off).min(n as u64) as usize;
        if durable_avail > 0 {
            let src = d_base + file_off as usize;
            dst[..durable_avail].copy_from_slice(&self.durable_data[src..src + durable_avail]);
        }
        if durable_avail < n {
            dst[durable_avail..].fill(0);
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
        if file_off < self.inodes[ii].durable_size {
            let d_idx =
                self.inodes[ii].data_idx as usize * self.max_bytes_per_file + file_off as usize;
            self.durable_data[d_idx] ^= mask;
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
            if entry.poisoned {
                continue;
            }
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
    }
}
