// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Preallocated unsynced write log, writeback eviction, and sync fold for [`SimDisk`].

use super::{DiskFaultCtx, SimDisk};
use crate::io::posix::errno::linux::EIO;

#[derive(Clone, Copy)]
pub(super) enum LogKind {
    Empty,
    Write {
        file_off: u64,
        len: u32,
        buf_off: usize,
    },
    Punch {
        file_off: u64,
        len: u64,
    },
    Truncate,
    Extend {
        end: u64,
    },
}

#[derive(Clone, Copy)]
pub(super) struct WriteLogEntry {
    pub(super) inode: u32,
    pub(super) kind: LogKind,
}

impl WriteLogEntry {
    pub(super) const EMPTY: Self = Self {
        inode: 0,
        kind: LogKind::Empty,
    };

    fn byte_len(self) -> usize {
        match self.kind {
            LogKind::Write { len, .. } => len as usize,
            LogKind::Empty | LogKind::Punch { .. } | LogKind::Truncate | LogKind::Extend { .. } => {
                0
            }
        }
    }
}

impl SimDisk {
    pub(super) fn live_size(&self, ii: usize) -> u64 {
        let di = self.inodes[ii].data_idx as usize;
        let mut sz = self.poisoned_eof[di].unwrap_or(self.inodes[ii].durable_size);
        for entry in &self.log_entries[..self.log_len] {
            if entry.inode as usize != ii {
                continue;
            }
            match entry.kind {
                LogKind::Write { file_off, len, .. } => {
                    sz = sz.max(file_off + u64::from(len));
                }
                LogKind::Truncate => sz = 0,
                LogKind::Extend { end } => sz = sz.max(end),
                LogKind::Punch { .. } | LogKind::Empty => {}
            }
        }
        sz
    }

    pub(super) fn push_write_log(&mut self, ii: usize, file_off: u64, bytes: &[u8]) {
        self.push_log_entry(
            ii,
            LogKind::Write {
                file_off,
                len: bytes.len() as u32,
                buf_off: 0,
            },
            bytes,
        );
    }

    pub(super) fn push_log_entry(&mut self, ii: usize, kind: LogKind, bytes: &[u8]) {
        let need = bytes.len();
        let cap_bytes = self.log_bytes.len();
        if self.max_log_entries == 0 || need > cap_bytes {
            self.flush_clean_entries_for_inode(ii);
            self.apply_direct_to_durable(ii, kind, bytes);
            return;
        }
        while self.log_len >= self.max_log_entries || self.log_bytes_used + need > cap_bytes {
            self.evict_log_entry(0);
        }
        let buf_off = if need > 0 {
            if self.log_arena_tail + need > cap_bytes {
                self.compact_log_bytes();
            }
            let off = self.log_arena_tail;
            self.log_bytes[off..off + need].copy_from_slice(bytes);
            self.log_arena_tail += need;
            self.log_bytes_used += need;
            off
        } else {
            0
        };
        let stored_kind = match kind {
            LogKind::Write { file_off, len, .. } => LogKind::Write {
                file_off,
                len,
                buf_off,
            },
            other @ (LogKind::Empty
            | LogKind::Punch { .. }
            | LogKind::Truncate
            | LogKind::Extend { .. }) => other,
        };
        self.log_entries[self.log_len] = WriteLogEntry {
            inode: ii as u32,
            kind: stored_kind,
        };
        self.log_len += 1;
    }

    pub(super) fn flush_clean_entries_for_inode(&mut self, ii: usize) {
        let mut i = 0;
        while i < self.log_len {
            if self.log_entries[i].inode as usize == ii {
                self.evict_log_entry(i);
            } else {
                i += 1;
            }
        }
    }

    fn evict_log_entry(&mut self, idx: usize) {
        let entry = self.log_entries[idx];
        let ii = entry.inode as usize;
        self.apply_kind_to_durable(ii, entry.kind);
        self.remove_log_entry(idx);
    }

    fn apply_direct_to_durable(&mut self, ii: usize, kind: LogKind, bytes: &[u8]) {
        match kind {
            LogKind::Write { file_off, .. } => {
                self.write_bytes_to_durable(ii, file_off, bytes);
            }
            other @ (LogKind::Empty
            | LogKind::Punch { .. }
            | LogKind::Truncate
            | LogKind::Extend { .. }) => self.apply_kind_to_durable(ii, other),
        }
    }

    pub(super) fn remove_log_entry(&mut self, idx: usize) {
        let b = self.log_entries[idx].byte_len();
        self.log_bytes_used = self.log_bytes_used.saturating_sub(b);
        self.log_entries.copy_within(idx + 1..self.log_len, idx);
        self.log_len -= 1;
    }

    fn compact_log_bytes(&mut self) {
        let mut cursor = 0usize;
        for entry in &mut self.log_entries[..self.log_len] {
            if let LogKind::Write { len, buf_off, .. } = &mut entry.kind {
                let n = *len as usize;
                if *buf_off != cursor {
                    self.log_bytes.copy_within(*buf_off..*buf_off + n, cursor);
                    *buf_off = cursor;
                }
                cursor += n;
            }
        }
        self.log_arena_tail = cursor;
        self.log_bytes_used = cursor;
    }

    fn write_bytes_to_durable(&mut self, ii: usize, file_off: u64, bytes: &[u8]) {
        let di = self.inodes[ii].data_idx as usize;
        let d_base = di * self.max_bytes_per_file;
        let old_sz = self.inodes[ii].durable_size;
        if !bytes.is_empty() && file_off > old_sz {
            self.durable_data[d_base + old_sz as usize..d_base + file_off as usize].fill(0);
        }
        let dst = d_base + file_off as usize;
        self.durable_data[dst..dst + bytes.len()].copy_from_slice(bytes);
        let end = file_off + bytes.len() as u64;
        if end > old_sz {
            self.inodes[ii].durable_size = end;
        }
        if !bytes.is_empty() {
            self.clear_poisoned_range(di, file_off as usize, end as usize);
            if let Some(pe) = self.poisoned_eof[di] {
                self.poisoned_eof[di] = Some(pe.max(end));
            }
        }
    }

    pub(super) fn apply_kind_to_durable(&mut self, ii: usize, kind: LogKind) {
        let di = self.inodes[ii].data_idx as usize;
        let d_base = di * self.max_bytes_per_file;
        match kind {
            LogKind::Write {
                file_off,
                len,
                buf_off,
            } => {
                let n = len as usize;
                let old_sz = self.inodes[ii].durable_size;
                if n > 0 && file_off > old_sz {
                    self.durable_data[d_base + old_sz as usize..d_base + file_off as usize].fill(0);
                }
                let dst = d_base + file_off as usize;
                self.durable_data[dst..dst + n]
                    .copy_from_slice(&self.log_bytes[buf_off..buf_off + n]);
                let end = file_off + n as u64;
                if end > old_sz {
                    self.inodes[ii].durable_size = end;
                }
                if n > 0 {
                    self.clear_poisoned_range(di, file_off as usize, end as usize);
                    if let Some(pe) = self.poisoned_eof[di] {
                        self.poisoned_eof[di] = Some(pe.max(end));
                    }
                }
            }
            LogKind::Punch { file_off, len } => {
                let sz = self.inodes[ii].durable_size;
                let s = file_off.min(sz) as usize;
                let e = file_off.saturating_add(len).min(sz) as usize;
                if e > s {
                    self.durable_data[d_base + s..d_base + e].fill(0);
                }
                let p_end = file_off
                    .saturating_add(len)
                    .min(self.max_bytes_per_file as u64) as usize;
                let p_start = file_off.min(p_end as u64) as usize;
                self.clear_poisoned_range(di, p_start, p_end);
            }
            LogKind::Truncate => {
                let sz = self.inodes[ii].durable_size as usize;
                if sz > 0 {
                    self.durable_data[d_base..d_base + sz].fill(0);
                }
                self.inodes[ii].durable_size = 0;
                self.clear_poisoned_inode(di);
            }
            LogKind::Extend { end } => {
                let sz = self.inodes[ii].durable_size;
                if end > sz {
                    self.durable_data[d_base + sz as usize..d_base + end as usize].fill(0);
                    self.inodes[ii].durable_size = end;
                }
                if let Some(pe) = self.poisoned_eof[di] {
                    self.poisoned_eof[di] = Some(pe.max(end));
                }
            }
            LogKind::Empty => {}
        }
    }

    pub(super) fn sync_file(&mut self, ii: usize) {
        self.flush_clean_entries_for_inode(ii);
    }

    /// Ends a write that reported `res` bytes to a file opened with `dsync`:
    /// what the file has buffered is made durable before the completion, as
    /// a data sync after the write would. The sync half can fail like any
    /// other sync ([`super::DiskFaults::sync_fail`]).
    pub(super) fn settle_dsync_write(
        &mut self,
        ii: usize,
        res: i32,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        if ctx.rng.chance(ctx.faults.sync_fail) {
            self.poison_file(ii);
            ctx.counters.sync_fail += 1;
            return -EIO;
        }
        self.sync_file(ii);
        res
    }

    pub(super) fn poison_file(&mut self, ii: usize) {
        let di = self.inodes[ii].data_idx as usize;
        let mut i = 0;
        while i < self.log_len {
            let entry = self.log_entries[i];
            if entry.inode as usize == ii {
                self.fold_entry_to_poisoned(di, ii, entry.kind);
                self.remove_log_entry(i);
            } else {
                i += 1;
            }
        }
    }

    pub(super) fn purge_inode_log(&mut self, ii: usize) {
        let mut i = 0;
        while i < self.log_len {
            if self.log_entries[i].inode as usize == ii {
                self.remove_log_entry(i);
            } else {
                i += 1;
            }
        }
    }
}
