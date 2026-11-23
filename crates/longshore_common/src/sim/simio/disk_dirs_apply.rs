// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Durable namespace replay helpers for [`super::dirs::DirChange`].

use super::dirs::DirChangeKind;
use super::{SimDisk, SimName};

#[derive(Clone, Copy)]
pub(super) struct ApplyReclaim {
    pub(super) overwritten: Option<usize>,
    pub(super) scan_all: bool,
}

impl ApplyReclaim {
    const NONE: Self = Self {
        overwritten: None,
        scan_all: false,
    };
}

fn durable_slice<'a>(
    names: &[SimName],
    name_bytes: &'a [u8],
    max_len: usize,
    i: usize,
) -> &'a [u8] {
    let base = i * max_len;
    &name_bytes[base..base + names[i].len as usize]
}

fn find_durable_by_inode(names: &[SimName], inode: u32) -> Option<usize> {
    (0..names.len()).find(|&i| names[i].used && names[i].inode == inode)
}

fn build_durable_path(
    names: &[SimName],
    name_bytes: &[u8],
    max_len: usize,
    parent_ii: u32,
    base: &[u8],
    dst: &mut [u8],
) -> Option<usize> {
    let p_ni = find_durable_by_inode(names, parent_ii)?;
    let p_len = names[p_ni].len as usize;
    let p_off = p_ni * max_len;
    if p_len == 1 && name_bytes[p_off] == b'/' {
        let total = 1 + base.len();
        if total > dst.len() {
            return None;
        }
        dst[0] = b'/';
        dst[1..total].copy_from_slice(base);
        Some(total)
    } else {
        let total = p_len + 1 + base.len();
        if total > dst.len() {
            return None;
        }
        dst[..p_len].copy_from_slice(&name_bytes[p_off..p_off + p_len]);
        dst[p_len] = b'/';
        dst[p_len + 1..total].copy_from_slice(base);
        Some(total)
    }
}

impl SimDisk {
    pub(super) fn find_durable_inode(&self, inode: u32) -> Option<usize> {
        find_durable_by_inode(&self.durable_names, inode)
    }

    fn find_durable_name(&self, path: &[u8]) -> Option<usize> {
        (0..self.durable_names.len()).find(|&i| {
            self.durable_names[i].used
                && durable_slice(
                    &self.durable_names,
                    &self.durable_name_bytes,
                    self.max_name_len,
                    i,
                ) == path
        })
    }

    pub(super) fn reclaim_dir_inodes(&mut self, refs: [Option<u32>; 4], reclaim: ApplyReclaim) {
        if reclaim.scan_all {
            for ii in 0..self.inodes.len() {
                self.free_inode_if_dead(ii);
            }
            return;
        }
        for r in refs.into_iter().flatten() {
            self.free_inode_if_dead(r as usize);
        }
        if let Some(ii) = reclaim.overwritten {
            self.free_inode_if_dead(ii);
        }
    }

    pub(super) fn apply_dir_change_to_durable(&mut self, idx: usize) -> ApplyReclaim {
        let e = self.dir_log[idx];
        let base_off = e.arena_slot as usize * 2 * self.max_name_len;
        match e.kind {
            DirChangeKind::Empty => ApplyReclaim::NONE,
            DirChangeKind::Create {
                parent,
                child,
                name_len,
            }
            | DirChangeKind::Mkdir {
                parent,
                child,
                name_len,
            } => match self.durable_path(parent, base_off, name_len, false) {
                Some(full_len) => self.apply_durable_create(child, full_len),
                None => ApplyReclaim::NONE,
            },
            DirChangeKind::Unlink {
                parent,
                child,
                name_len,
            } => {
                if let Some(full_len) = self.durable_path(parent, base_off, name_len, false) {
                    self.apply_durable_unlink(child, full_len);
                }
                ApplyReclaim::NONE
            }
            DirChangeKind::Rename {
                old_parent,
                new_parent,
                moved,
                old_len,
                new_len,
                ..
            } => {
                let new_off = base_off + self.max_name_len;
                let Some(old_full_len) = self.durable_path(old_parent, base_off, old_len, false)
                else {
                    return ApplyReclaim::NONE;
                };
                let Some(new_full_len) = self.durable_path(new_parent, new_off, new_len, true)
                else {
                    return ApplyReclaim::NONE;
                };
                self.apply_durable_rename(moved, old_full_len, new_full_len)
            }
        }
    }

    /// Builds the durable full path of the arena name at `off..off + len`
    /// under directory inode `parent`, into scratch B if `into_b`, else A.
    fn durable_path(&mut self, parent: u32, off: usize, len: u16, into_b: bool) -> Option<usize> {
        let base = &self.dir_name_arena[off..off + len as usize];
        let dst = if into_b {
            &mut self.durable_scratch_b
        } else {
            &mut self.durable_scratch_a
        };
        build_durable_path(
            &self.durable_names,
            &self.durable_name_bytes,
            self.max_name_len,
            parent,
            base,
            dst,
        )
    }

    /// Links `child` durably at the path in scratch A, replacing whatever
    /// the path named before.
    fn apply_durable_create(&mut self, child: u32, full_len: usize) -> ApplyReclaim {
        if let Some(old_ni) = self.find_durable_name(&self.durable_scratch_a[..full_len]) {
            let old_ii = self.durable_names[old_ni].inode as usize;
            if old_ii == child as usize {
                return ApplyReclaim::NONE;
            }
            let scan_all = if self.inodes[old_ii].is_dir {
                self.clear_durable_descendants_a(old_ni, full_len)
            } else {
                false
            };
            self.inodes[old_ii].durable_links = self.inodes[old_ii].durable_links.saturating_sub(1);
            self.durable_names[old_ni].inode = child;
            self.inodes[child as usize].durable_links += 1;
            return ApplyReclaim {
                overwritten: Some(old_ii),
                scan_all,
            };
        }
        let Some(ni) = self.durable_names.iter().position(|n| !n.used) else {
            return ApplyReclaim::NONE;
        };
        let dst = ni * self.max_name_len;
        self.durable_name_bytes[dst..dst + full_len]
            .copy_from_slice(&self.durable_scratch_a[..full_len]);
        self.durable_names[ni] = SimName {
            used: true,
            len: full_len as u16,
            inode: child,
        };
        self.inodes[child as usize].durable_links += 1;
        ApplyReclaim::NONE
    }

    /// Removes the durable name in scratch A if it still names `child`.
    fn apply_durable_unlink(&mut self, child: u32, full_len: usize) {
        let Some(ni) = self.find_durable_name(&self.durable_scratch_a[..full_len]) else {
            return;
        };
        if self.durable_names[ni].inode == child {
            self.durable_names[ni].used = false;
            self.inodes[child as usize].durable_links =
                self.inodes[child as usize].durable_links.saturating_sub(1);
        }
    }

    fn apply_durable_rename(
        &mut self,
        moved: u32,
        old_full_len: usize,
        new_full_len: usize,
    ) -> ApplyReclaim {
        let old_full = &self.durable_scratch_a[..old_full_len];
        let new_full = &self.durable_scratch_b[..new_full_len];
        if old_full == new_full {
            return ApplyReclaim::NONE;
        }
        let Some(old_ni) = self.find_durable_name(old_full) else {
            return ApplyReclaim::NONE;
        };
        if self.durable_names[old_ni].inode != moved {
            return ApplyReclaim::NONE;
        }
        let moved_is_dir = self.inodes[moved as usize].is_dir;
        if moved_is_dir
            && new_full_len > old_full_len
            && &new_full[..old_full_len] == old_full
            && new_full[old_full_len] == b'/'
        {
            return ApplyReclaim::NONE;
        }
        let dst = self.find_durable_name(new_full);
        let mut reclaim = ApplyReclaim::NONE;
        if let Some(dst_ni) = dst {
            let dst_ii = self.durable_names[dst_ni].inode as usize;
            self.durable_names[dst_ni].used = false;
            self.inodes[dst_ii].durable_links = self.inodes[dst_ii].durable_links.saturating_sub(1);
            reclaim.overwritten = Some(dst_ii);
            if self.inodes[dst_ii].is_dir {
                reclaim.scan_all = self.clear_durable_descendants_b(
                    dst_ni,
                    old_ni,
                    old_full_len,
                    new_full_len,
                    moved_is_dir,
                );
            }
        }
        if moved_is_dir && self.rewrite_durable_children(old_ni, old_full_len, new_full_len) {
            reclaim.scan_all = true;
        }
        let d_off = old_ni * self.max_name_len;
        self.durable_name_bytes[d_off..d_off + new_full_len]
            .copy_from_slice(&self.durable_scratch_b[..new_full_len]);
        self.durable_names[old_ni].len = new_full_len as u16;
        reclaim
    }

    fn clear_durable_descendants_a(&mut self, dir_ni: usize, dir_len: usize) -> bool {
        let mut cleared = false;
        for k in 0..self.durable_names.len() {
            if !self.durable_names[k].used || k == dir_ni {
                continue;
            }
            let s_len = self.durable_names[k].len as usize;
            let base = k * self.max_name_len;
            let is_child = s_len > dir_len
                && self.durable_name_bytes[base..base + dir_len]
                    == self.durable_scratch_a[..dir_len]
                && self.durable_name_bytes[base + dir_len] == b'/';
            if is_child {
                let ii = self.durable_names[k].inode as usize;
                self.durable_names[k].used = false;
                self.inodes[ii].durable_links = self.inodes[ii].durable_links.saturating_sub(1);
                cleared = true;
            }
        }
        cleared
    }

    fn clear_durable_descendants_b(
        &mut self,
        dst_ni: usize,
        old_ni: usize,
        old_len: usize,
        new_len: usize,
        moved_is_dir: bool,
    ) -> bool {
        let mut cleared = false;
        for k in 0..self.durable_names.len() {
            if !self.durable_names[k].used || k == dst_ni || k == old_ni {
                continue;
            }
            let s_len = self.durable_names[k].len as usize;
            let base = k * self.max_name_len;
            let in_dst = s_len > new_len
                && self.durable_name_bytes[base..base + new_len]
                    == self.durable_scratch_b[..new_len]
                && self.durable_name_bytes[base + new_len] == b'/';
            if !in_dst {
                continue;
            }
            let in_old = moved_is_dir
                && s_len > old_len
                && self.durable_name_bytes[base..base + old_len]
                    == self.durable_scratch_a[..old_len]
                && self.durable_name_bytes[base + old_len] == b'/';
            if in_old {
                continue;
            }
            let ii = self.durable_names[k].inode as usize;
            self.durable_names[k].used = false;
            self.inodes[ii].durable_links = self.inodes[ii].durable_links.saturating_sub(1);
            cleared = true;
        }
        cleared
    }

    fn rewrite_durable_children(&mut self, old_ni: usize, old_len: usize, new_len: usize) -> bool {
        let mut dropped = false;
        for k in 0..self.durable_names.len() {
            if !self.durable_names[k].used || k == old_ni {
                continue;
            }
            let s_len = self.durable_names[k].len as usize;
            let base = k * self.max_name_len;
            let is_child = s_len > old_len
                && self.durable_name_bytes[base..base + old_len]
                    == self.durable_scratch_a[..old_len]
                && self.durable_name_bytes[base + old_len] == b'/';
            if is_child {
                let tail = s_len - old_len;
                if new_len + tail > self.max_name_len {
                    let ii = self.durable_names[k].inode as usize;
                    self.durable_names[k].used = false;
                    self.inodes[ii].durable_links = self.inodes[ii].durable_links.saturating_sub(1);
                    dropped = true;
                } else {
                    self.durable_name_bytes
                        .copy_within(base + old_len..base + s_len, base + new_len);
                    self.durable_name_bytes[base..base + new_len]
                        .copy_from_slice(&self.durable_scratch_b[..new_len]);
                    self.durable_names[k].len = (new_len + tail) as u16;
                }
            }
        }
        dropped
    }
}
