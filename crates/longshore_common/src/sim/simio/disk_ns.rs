// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Namespace-mutating directory operations (`rename`, `unlink`, `mkdir`) for [`SimDisk`].

use super::dirs::DirChangeKind;
use super::paths::base_range;
use super::{SimDisk, SimName};
use crate::io::posix::errno::linux::{
    EBUSY, EEXIST, EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY,
};

impl SimDisk {
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
        let new_parent_ii = match self.check_parent(new) {
            Ok(p) => p,
            Err(e) => return e,
        };
        if old == new {
            return 0;
        }
        let old_parent_ii = self.check_parent(old).unwrap_or(0);
        let old_ii = self.names[old_ni].inode as usize;
        let old_is_dir = self.inodes[old_ii].is_dir;
        if old_is_dir && new.len() > old.len() && &new[..old.len()] == old && new[old.len()] == b'/'
        {
            return -EINVAL;
        }
        let dst = self.find_name(new);
        if let Err(e) = self.check_rename_dst(dst, old_is_dir, new_len) {
            return e;
        }
        if old_is_dir && !self.children_fit(old_len, new_len) {
            return -ENAMETOOLONG;
        }
        let mut replaced_ii = None;
        if let Some(dst_ni) = dst {
            let dst_ii = self.names[dst_ni].inode as usize;
            self.names[dst_ni].used = false;
            self.inodes[dst_ii].live_links = self.inodes[dst_ii].live_links.saturating_sub(1);
            replaced_ii = Some(dst_ii);
        }
        if old_is_dir {
            self.move_children(old_ni, old_len, new_len);
        }
        let base = old_ni * self.max_name_len;
        self.name_bytes[base..base + new_len].copy_from_slice(&self.scratch_b[..new_len]);
        self.names[old_ni].len = new_len as u16;
        let oa_range = base_range(&self.scratch_a[..old_len]);
        let nb_range = base_range(&self.scratch_b[..new_len]);
        self.record_dir_change(
            DirChangeKind::Rename {
                old_parent: old_parent_ii,
                new_parent: new_parent_ii,
                moved: old_ii as u32,
                replaced: replaced_ii.map(|r| r as u32),
                old_len: oa_range.len() as u16,
                new_len: nb_range.len() as u16,
            },
            oa_range,
            nb_range,
        );
        if let Some(dst_ii) = replaced_ii {
            self.free_inode_if_dead(dst_ii);
        }
        0
    }

    /// Refuses a rename onto name `dst` (the `new_len`-byte path in scratch
    /// B) that swaps a directory for a file or replaces a non-empty one.
    fn check_rename_dst(
        &self,
        dst: Option<usize>,
        old_is_dir: bool,
        new_len: usize,
    ) -> Result<(), i32> {
        let Some(dst_ni) = dst else {
            return Ok(());
        };
        let new = &self.scratch_b[..new_len];
        let dst_ii = self.names[dst_ni].inode as usize;
        let dst_is_dir = self.inodes[dst_ii].is_dir;
        if old_is_dir && !dst_is_dir {
            return Err(-ENOTDIR);
        }
        if !old_is_dir && dst_is_dir {
            return Err(-EISDIR);
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
                return Err(-ENOTEMPTY);
            }
        }
        Ok(())
    }

    /// Whether every name under the directory in scratch A (`old_len` bytes)
    /// still fits once that prefix becomes the `new_len`-byte path.
    fn children_fit(&self, old_len: usize, new_len: usize) -> bool {
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
                return false;
            }
        }
        true
    }

    /// Rewrites the names under the directory in scratch A to sit under the
    /// path in scratch B.
    fn move_children(&mut self, old_ni: usize, old_len: usize, new_len: usize) {
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
                self.name_bytes[base..base + new_len].copy_from_slice(&self.scratch_b[..new_len]);
                self.names[k].len = (new_len + tail) as u16;
            }
        }
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
        let parent_ii = self.check_parent(&self.scratch_a[..len]).unwrap_or(0);
        self.names[ni].used = false;
        self.inodes[ii].live_links = self.inodes[ii].live_links.saturating_sub(1);
        let a_range = base_range(&self.scratch_a[..len]);
        self.record_dir_change(
            DirChangeKind::Unlink {
                parent: parent_ii,
                child: ii as u32,
                name_len: a_range.len() as u16,
            },
            a_range,
            0..0,
        );
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
        let parent_ii = match self.check_parent(&self.scratch_a[..len]) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let ii = match self.alloc_dir_inode() {
            Ok(i) => i,
            Err(e) => return e,
        };
        let ni = match self.names.iter().position(|n| !n.used) {
            Some(i) => i,
            None => {
                self.inodes[ii].live_links = 0;
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
        let a_range = base_range(&self.scratch_a[..len]);
        self.record_dir_change(
            DirChangeKind::Mkdir {
                parent: parent_ii,
                child: ii as u32,
                name_len: a_range.len() as u16,
            },
            a_range,
            0..0,
        );
        0
    }
}
