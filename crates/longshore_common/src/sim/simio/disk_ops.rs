// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Read, write, space, status, and directory/name operations for [`SimDisk`].

use super::{SimDisk, SimName};
use crate::io::files::STATX_SIZE;
use crate::io::posix::errno::linux::{
    EBADF, EBUSY, EEXIST, EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY,
};
use crate::io::FileSlot;

impl SimDisk {
    pub(super) fn read(
        &self,
        slot: FileSlot,
        pool: &mut [u8],
        range: core::ops::Range<usize>,
        file_off: u64,
    ) -> i32 {
        let Some(open) = self.open_slots.get(slot.0 as usize).and_then(|s| *s) else {
            return -EBADF;
        };
        let ino = &self.inodes[open.inode as usize];
        if ino.is_dir {
            return -EISDIR;
        }
        if !open.flags.read && open.flags.write {
            return -EBADF;
        }
        if file_off >= ino.size || range.is_empty() {
            return 0;
        }
        let n = range.len().min((ino.size - file_off) as usize);
        let d_off = ino.data_idx as usize * self.max_bytes_per_file + file_off as usize;
        pool[range.start..range.start + n].copy_from_slice(&self.file_data[d_off..d_off + n]);
        n as i32
    }

    pub(super) fn write(
        &mut self,
        slot: FileSlot,
        pool: &[u8],
        range: core::ops::Range<usize>,
        file_off: u64,
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
        let d_base = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
        let old_size = self.inodes[ii].size;
        if len > 0 && file_off > old_size {
            self.file_data[d_base + old_size as usize..d_base + file_off as usize].fill(0);
        }
        let dst = d_base + file_off as usize;
        self.file_data[dst..dst + range.len()].copy_from_slice(&pool[range.clone()]);
        if end > old_size {
            self.inodes[ii].size = end;
        }
        range.len() as i32
    }

    pub(super) fn reserve(
        &mut self,
        slot: FileSlot,
        file_off: u64,
        len: u64,
        keep_size: bool,
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
        let old_size = self.inodes[ii].size;
        if !keep_size && end > old_size {
            let d_base = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
            self.file_data[d_base + old_size as usize..d_base + end as usize].fill(0);
            self.inodes[ii].size = end;
        }
        0
    }

    pub(super) fn punch(&mut self, slot: FileSlot, file_off: u64, len: u64) -> i32 {
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
        let size = self.inodes[ii].size;
        let start = file_off.min(size) as usize;
        let end = file_off.saturating_add(len).min(size) as usize;
        if end > start {
            let d_base = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
            self.file_data[d_base + start..d_base + end].fill(0);
        }
        0
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
        let ino = &self.inodes[self.names[ni].inode as usize];
        let mut raw = [0u8; STATX_SIZE];
        let mode: u16 = if ino.is_dir { 0o040755 } else { 0o100644 };
        raw[0..4].copy_from_slice(&0x07ffu32.to_le_bytes());
        raw[4..8].copy_from_slice(&4096u32.to_le_bytes());
        raw[16..20].copy_from_slice(&ino.links.to_le_bytes());
        raw[28..30].copy_from_slice(&mode.to_le_bytes());
        raw[32..40].copy_from_slice(&ino.ino.to_le_bytes());
        raw[40..48].copy_from_slice(&ino.size.to_le_bytes());
        raw[48..56].copy_from_slice(&ino.size.div_ceil(512).to_le_bytes());
        pool[out_range].copy_from_slice(&raw);
        0
    }

    pub(super) fn rename(
        &mut self,
        pool: &[u8],
        old_range: core::ops::Range<usize>,
        new_range: core::ops::Range<usize>,
    ) -> i32 {
        let old_len = match Self::normalize_into(&pool[old_range], &mut self.scratch_a) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let new_len = match Self::normalize_into(&pool[new_range], &mut self.scratch_b) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let old = &self.scratch_a[..old_len];
        let new = &self.scratch_b[..new_len];
        if old == b"/" || new == b"/" {
            return -EBUSY;
        }
        let Some(old_ni) = self.find_name(old) else {
            return -ENOENT;
        };
        if let Err(e) = self.check_parent(new) {
            return e;
        }
        if old == new {
            return 0;
        }
        let old_ii = self.names[old_ni].inode as usize;
        let old_is_dir = self.inodes[old_ii].is_dir;
        if old_is_dir && new.len() > old.len() && &new[..old.len()] == old && new[old.len()] == b'/'
        {
            return -EINVAL;
        }
        let dst = self.find_name(new);
        if let Some(dst_ni) = dst {
            let dst_ii = self.names[dst_ni].inode as usize;
            let dst_is_dir = self.inodes[dst_ii].is_dir;
            if old_is_dir && !dst_is_dir {
                return -ENOTDIR;
            }
            if !old_is_dir && dst_is_dir {
                return -EISDIR;
            }
            if dst_is_dir {
                let has_child = (0..self.names.len()).any(|k| {
                    if !self.names[k].used {
                        return false;
                    }
                    let s = self.name_slice(k);
                    s.len() > new.len() && &s[..new.len()] == new && s[new.len()] == b'/'
                });
                if has_child {
                    return -ENOTEMPTY;
                }
            }
        }
        if old_is_dir {
            for k in 0..self.names.len() {
                if !self.names[k].used {
                    continue;
                }
                let s_len = self.names[k].len as usize;
                let base = k * self.max_name_len;
                let is_child = s_len > old_len
                    && self.name_bytes[base..base + old_len] == self.scratch_a[..old_len]
                    && self.name_bytes[base + old_len] == b'/';
                if is_child && new_len + (s_len - old_len) > self.max_name_len {
                    return -ENAMETOOLONG;
                }
            }
        }
        if let Some(dst_ni) = dst {
            let dst_ii = self.names[dst_ni].inode as usize;
            self.names[dst_ni].used = false;
            self.inodes[dst_ii].links = self.inodes[dst_ii].links.saturating_sub(1);
            self.free_inode_if_dead(dst_ii);
        }
        if old_is_dir {
            for k in 0..self.names.len() {
                if !self.names[k].used || k == old_ni {
                    continue;
                }
                let s_len = self.names[k].len as usize;
                let base = k * self.max_name_len;
                let is_child = s_len > old_len
                    && self.name_bytes[base..base + old_len] == self.scratch_a[..old_len]
                    && self.name_bytes[base + old_len] == b'/';
                if is_child {
                    let tail = s_len - old_len;
                    self.name_bytes
                        .copy_within(base + old_len..base + s_len, base + new_len);
                    self.name_bytes[base..base + new_len]
                        .copy_from_slice(&self.scratch_b[..new_len]);
                    self.names[k].len = (new_len + tail) as u16;
                }
            }
        }
        let base = old_ni * self.max_name_len;
        self.name_bytes[base..base + new_len].copy_from_slice(&self.scratch_b[..new_len]);
        self.names[old_ni].len = new_len as u16;
        0
    }

    pub(super) fn unlink(&mut self, raw_path: &[u8]) -> i32 {
        let len = match Self::normalize_into(raw_path, &mut self.scratch_a) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if &self.scratch_a[..len] == b"/" {
            return -EISDIR;
        }
        let Some(ni) = self.find_name(&self.scratch_a[..len]) else {
            return -ENOENT;
        };
        let ii = self.names[ni].inode as usize;
        if self.inodes[ii].is_dir {
            return -EISDIR;
        }
        self.names[ni].used = false;
        self.inodes[ii].links = self.inodes[ii].links.saturating_sub(1);
        self.free_inode_if_dead(ii);
        0
    }

    pub(super) fn mkdir(&mut self, raw_path: &[u8]) -> i32 {
        let len = match Self::normalize_into(raw_path, &mut self.scratch_a) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if self.find_name(&self.scratch_a[..len]).is_some() {
            return -EEXIST;
        }
        if let Err(e) = self.check_parent(&self.scratch_a[..len]) {
            return e;
        }
        let ii = match self.alloc_dir_inode() {
            Ok(i) => i,
            Err(e) => return e,
        };
        let ni = match self.names.iter().position(|n| !n.used) {
            Some(i) => i,
            None => {
                self.inodes[ii].links = 0;
                self.free_inode_if_dead(ii);
                return -ENOSPC;
            }
        };
        let base = ni * self.max_name_len;
        self.name_bytes[base..base + len].copy_from_slice(&self.scratch_a[..len]);
        self.names[ni] = SimName {
            used: true,
            len: len as u16,
            inode: ii as u32,
        };
        0
    }
}
