// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Path normalization, inode/name allocation, `open`, and `close` for [`SimDisk`].

use super::{SimDisk, SimInode, SimName, SimOpenFile};
use crate::io::posix::errno::linux::{
    EBADF, EEXIST, EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOSPC, ENOTDIR,
};
use crate::io::{FileSlot, OpenFlags};

impl SimDisk {
    pub(super) fn normalize_into(raw: &[u8], dst: &mut [u8]) -> Result<usize, i32> {
        let trimmed = raw.strip_suffix(&[0]).unwrap_or(raw);
        if trimmed.is_empty() || dst.is_empty() {
            return Err(-EINVAL);
        }
        let mut out = 0usize;
        if trimmed[0] != b'/' {
            dst[0] = b'/';
            out = 1;
        }
        let mut prev_slash = false;
        for &b in trimmed {
            if b == b'/' && prev_slash {
                continue;
            }
            prev_slash = b == b'/';
            if out >= dst.len() {
                return Err(-ENAMETOOLONG);
            }
            dst[out] = b;
            out += 1;
        }
        while out > 1 && dst[out - 1] == b'/' {
            out -= 1;
        }
        Ok(out)
    }

    pub(super) fn name_slice(&self, i: usize) -> &[u8] {
        let base = i * self.max_name_len;
        &self.name_bytes[base..base + self.names[i].len as usize]
    }

    pub(super) fn find_name(&self, path: &[u8]) -> Option<usize> {
        (0..self.names.len()).find(|&i| self.names[i].used && self.name_slice(i) == path)
    }

    pub(super) fn check_parent(&self, path: &[u8]) -> Result<(), i32> {
        let parent = match path.iter().rposition(|&b| b == b'/') {
            Some(0) | None => b"/" as &[u8],
            Some(pos) => &path[..pos],
        };
        let pi = self.find_name(parent).ok_or(-ENOENT)?;
        if !self.inodes[self.names[pi].inode as usize].is_dir {
            return Err(-ENOTDIR);
        }
        Ok(())
    }

    pub(super) fn alloc_file_inode(&mut self) -> Result<usize, i32> {
        let di = self.data_used.iter().position(|&u| !u).ok_or(-ENOSPC)?;
        let ii = self.inodes.iter().position(|i| !i.used).ok_or(-ENOSPC)?;
        self.data_used[di] = true;
        let d_off = di * self.max_bytes_per_file;
        self.file_data[d_off..d_off + self.max_bytes_per_file].fill(0);
        let ino = self.next_ino;
        self.next_ino += 1;
        self.inodes[ii] = SimInode {
            used: true,
            is_dir: false,
            data_idx: di as u32,
            size: 0,
            links: 1,
            opens: 0,
            ino,
        };
        Ok(ii)
    }

    pub(super) fn alloc_dir_inode(&mut self) -> Result<usize, i32> {
        let ii = self.inodes.iter().position(|i| !i.used).ok_or(-ENOSPC)?;
        let ino = self.next_ino;
        self.next_ino += 1;
        self.inodes[ii] = SimInode {
            used: true,
            is_dir: true,
            data_idx: 0,
            size: 0,
            links: 1,
            opens: 0,
            ino,
        };
        Ok(ii)
    }

    pub(super) fn free_inode_if_dead(&mut self, ii: usize) {
        let ino = &mut self.inodes[ii];
        if ino.used && ino.links == 0 && ino.opens == 0 {
            if !ino.is_dir {
                self.data_used[ino.data_idx as usize] = false;
            }
            *ino = SimInode::EMPTY;
        }
    }

    pub(super) fn open(&mut self, slot: FileSlot, raw_path: &[u8], flags: OpenFlags) -> i32 {
        let len = match Self::normalize_into(raw_path, &mut self.scratch_a) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if let Some(ni) = self.find_name(&self.scratch_a[..len]) {
            if flags.create && flags.exclusive {
                return -EEXIST;
            }
            let ii = self.names[ni].inode as usize;
            if self.inodes[ii].is_dir {
                if flags.write || flags.empty || flags.create {
                    return -EISDIR;
                }
            } else {
                if flags.directory {
                    return -ENOTDIR;
                }
                if flags.empty {
                    let d_off = self.inodes[ii].data_idx as usize * self.max_bytes_per_file;
                    let old_sz = self.inodes[ii].size as usize;
                    self.file_data[d_off..d_off + old_sz].fill(0);
                    self.inodes[ii].size = 0;
                }
            }
            self.inodes[ii].opens += 1;
            self.open_slots[slot.0 as usize] = Some(SimOpenFile {
                inode: ii as u32,
                flags,
            });
            return 0;
        }
        if !flags.create || flags.directory {
            return -ENOENT;
        }
        if let Err(e) = self.check_parent(&self.scratch_a[..len]) {
            return e;
        }
        let ii = match self.alloc_file_inode() {
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
        self.inodes[ii].opens += 1;
        self.open_slots[slot.0 as usize] = Some(SimOpenFile {
            inode: ii as u32,
            flags,
        });
        0
    }

    pub(super) fn close(&mut self, slot: FileSlot) -> i32 {
        let Some(entry) = self
            .open_slots
            .get_mut(slot.0 as usize)
            .and_then(Option::take)
        else {
            return -EBADF;
        };
        let ii = entry.inode as usize;
        self.inodes[ii].opens = self.inodes[ii].opens.saturating_sub(1);
        self.free_inode_if_dead(ii);
        0
    }
}
