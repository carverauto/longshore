// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Preallocated unsynced write log, writeback eviction, and sync fold for [`SimDisk`].

use super::SimDisk;

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
    pub(super) poisoned: bool,
    pub(super) kind: LogKind,
}

impl WriteLogEntry {
    pub(super) const EMPTY: Self = Self {
        inode: 0,
        poisoned: false,
        kind: LogKind::Empty,
    };

    fn byte_len(self) -> usize {
        match self.kind {
            LogKind::Write { len, .. } => len as usize,
            _ => 0,
        }
    }
}

impl SimDisk {
    pub(super) fn live_size(&self, ii: usize) -> u64 {
        let mut sz = self.inodes[ii].durable_size;
        for entry in &self.log_entries[..self.log_len] {
            if entry.inode as usize != ii {
                continue;
            }
            match entry.kind {
                LogKind::Write { file_off, len, .. } => {
                    sz = sz.max(file_off + len as u64);
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
            if let Some(k) = (0..self.log_len).find(|&i| !self.log_entries[i].poisoned) {
                self.evict_log_entry(k);
            } else {
                self.apply_direct_to_durable(ii, kind, bytes);
                return;
            }
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
            other => other,
        };
        self.log_entries[self.log_len] = WriteLogEntry {
            inode: ii as u32,
            poisoned: false,
            kind: stored_kind,
        };
        self.log_len += 1;
    }

    pub(super) fn flush_clean_entries_for_inode(&mut self, ii: usize) {
        let mut i = 0;
        while i < self.log_len {
            let e = self.log_entries[i];
            if e.inode as usize == ii && !e.poisoned {
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
        self.trim_poisoned(ii, idx, entry.kind);
    }

    fn apply_direct_to_durable(&mut self, ii: usize, kind: LogKind, bytes: &[u8]) {
        match kind {
            LogKind::Write { file_off, .. } => {
                self.write_bytes_to_durable(ii, file_off, bytes);
            }
            other => self.apply_kind_to_durable(ii, other),
        }
        self.trim_poisoned(ii, self.log_len, kind);
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
        let d_base = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
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
    }

    pub(super) fn apply_kind_to_durable(&mut self, ii: usize, kind: LogKind) {
        let d_base = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
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
            }
            LogKind::Punch { file_off, len } => {
                let sz = self.inodes[ii].durable_size;
                let s = file_off.min(sz) as usize;
                let e = file_off.saturating_add(len).min(sz) as usize;
                if e > s {
                    self.durable_data[d_base + s..d_base + e].fill(0);
                }
            }
            LogKind::Truncate => {
                let sz = self.inodes[ii].durable_size as usize;
                if sz > 0 {
                    self.durable_data[d_base..d_base + sz].fill(0);
                }
                self.inodes[ii].durable_size = 0;
            }
            LogKind::Extend { end } => {
                let sz = self.inodes[ii].durable_size;
                if end > sz {
                    self.durable_data[d_base + sz as usize..d_base + end as usize].fill(0);
                    self.inodes[ii].durable_size = end;
                }
            }
            LogKind::Empty => {}
        }
    }

    pub(super) fn sync_file(&mut self, ii: usize) {
        self.flush_clean_entries_for_inode(ii);
    }

    pub(super) fn poison_file(&mut self, ii: usize) {
        for entry in &mut self.log_entries[..self.log_len] {
            if entry.inode as usize == ii && !entry.poisoned {
                entry.poisoned = true;
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
