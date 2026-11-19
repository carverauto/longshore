// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Durable namespace replay helpers for [`super::dirs::DirChange`].

use super::dirs::DirChangeKind;
use super::{SimDisk, SimName};

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

    pub(super) fn apply_dir_change_to_durable(&mut self, idx: usize) {
        let e = self.dir_log[idx];
        let base_off = e.arena_slot as usize * 2 * self.max_name_len;
        match e.kind {
            DirChangeKind::Empty => {}
            DirChangeKind::Create {
                parent,
                child,
                name_len,
            }
            | DirChangeKind::Mkdir {
                parent,
                child,
                name_len,
            } => {
                let base = &self.dir_name_arena[base_off..base_off + name_len as usize];
                let Some(full_len) = build_durable_path(
                    &self.durable_names,
                    &self.durable_name_bytes,
                    self.max_name_len,
                    parent,
                    base,
                    &mut self.scratch_a,
                ) else {
                    return;
                };
                if self
                    .find_durable_name(&self.scratch_a[..full_len])
                    .is_some()
                {
                    return;
                }
                let Some(ni) = self.durable_names.iter().position(|n| !n.used) else {
                    return;
                };
                let dst = ni * self.max_name_len;
                self.durable_name_bytes[dst..dst + full_len]
                    .copy_from_slice(&self.scratch_a[..full_len]);
                self.durable_names[ni] = SimName {
                    used: true,
                    len: full_len as u16,
                    inode: child,
                };
                self.inodes[child as usize].durable_links += 1;
            }
            DirChangeKind::Unlink {
                parent,
                child,
                name_len,
            } => {
                let base = &self.dir_name_arena[base_off..base_off + name_len as usize];
                let Some(full_len) = build_durable_path(
                    &self.durable_names,
                    &self.durable_name_bytes,
                    self.max_name_len,
                    parent,
                    base,
                    &mut self.scratch_a,
                ) else {
                    return;
                };
                let Some(ni) = self.find_durable_name(&self.scratch_a[..full_len]) else {
                    return;
                };
                if self.durable_names[ni].inode == child {
                    self.durable_names[ni].used = false;
                    self.inodes[child as usize].durable_links =
                        self.inodes[child as usize].durable_links.saturating_sub(1);
                }
            }
            DirChangeKind::Rename {
                old_parent,
                new_parent,
                moved,
                old_len,
                new_len,
                ..
            } => {
                let old_base = &self.dir_name_arena[base_off..base_off + old_len as usize];
                let new_off = base_off + self.max_name_len;
                let new_base = &self.dir_name_arena[new_off..new_off + new_len as usize];
                let Some(old_full_len) = build_durable_path(
                    &self.durable_names,
                    &self.durable_name_bytes,
                    self.max_name_len,
                    old_parent,
                    old_base,
                    &mut self.scratch_a,
                ) else {
                    return;
                };
                let Some(new_full_len) = build_durable_path(
                    &self.durable_names,
                    &self.durable_name_bytes,
                    self.max_name_len,
                    new_parent,
                    new_base,
                    &mut self.scratch_b,
                ) else {
                    return;
                };
                self.apply_durable_rename(moved, old_full_len, new_full_len);
            }
        }
    }

    fn apply_durable_rename(&mut self, moved: u32, old_full_len: usize, new_full_len: usize) {
        let old_full = &self.scratch_a[..old_full_len];
        let new_full = &self.scratch_b[..new_full_len];
        if old_full == new_full {
            return;
        }
        let Some(old_ni) = self.find_durable_name(old_full) else {
            return;
        };
        if self.durable_names[old_ni].inode != moved {
            return;
        }
        let moved_is_dir = self.inodes[moved as usize].is_dir;
        if moved_is_dir
            && new_full_len > old_full_len
            && &new_full[..old_full_len] == old_full
            && new_full[old_full_len] == b'/'
        {
            return;
        }
        let dst = self.find_durable_name(new_full);
        if let Some(dst_ni) = dst {
            let dst_ii = self.durable_names[dst_ni].inode as usize;
            let dst_is_dir = self.inodes[dst_ii].is_dir;
            if moved_is_dir != dst_is_dir {
                return;
            }
            if dst_is_dir && self.durable_dir_has_child(new_full) {
                return;
            }
        }
        if moved_is_dir && self.durable_children_exceed_len(old_full_len, new_full_len) {
            return;
        }
        if let Some(dst_ni) = dst {
            let dst_ii = self.durable_names[dst_ni].inode as usize;
            self.durable_names[dst_ni].used = false;
            self.inodes[dst_ii].durable_links = self.inodes[dst_ii].durable_links.saturating_sub(1);
        }
        if moved_is_dir {
            self.rewrite_durable_children(old_ni, old_full_len, new_full_len);
        }
        let d_off = old_ni * self.max_name_len;
        self.durable_name_bytes[d_off..d_off + new_full_len]
            .copy_from_slice(&self.scratch_b[..new_full_len]);
        self.durable_names[old_ni].len = new_full_len as u16;
    }

    fn durable_dir_has_child(&self, dir_path: &[u8]) -> bool {
        (0..self.durable_names.len()).any(|k| {
            if !self.durable_names[k].used {
                return false;
            }
            let s = durable_slice(
                &self.durable_names,
                &self.durable_name_bytes,
                self.max_name_len,
                k,
            );
            s.len() > dir_path.len()
                && &s[..dir_path.len()] == dir_path
                && s[dir_path.len()] == b'/'
        })
    }

    fn durable_children_exceed_len(&self, old_len: usize, new_len: usize) -> bool {
        for k in 0..self.durable_names.len() {
            if !self.durable_names[k].used {
                continue;
            }
            let s_len = self.durable_names[k].len as usize;
            let base = k * self.max_name_len;
            let is_child = s_len > old_len
                && self.durable_name_bytes[base..base + old_len] == self.scratch_a[..old_len]
                && self.durable_name_bytes[base + old_len] == b'/';
            if is_child && new_len + (s_len - old_len) > self.max_name_len {
                return true;
            }
        }
        false
    }

    fn rewrite_durable_children(&mut self, old_ni: usize, old_len: usize, new_len: usize) {
        for k in 0..self.durable_names.len() {
            if !self.durable_names[k].used || k == old_ni {
                continue;
            }
            let s_len = self.durable_names[k].len as usize;
            let base = k * self.max_name_len;
            let is_child = s_len > old_len
                && self.durable_name_bytes[base..base + old_len] == self.scratch_a[..old_len]
                && self.durable_name_bytes[base + old_len] == b'/';
            if is_child {
                let tail = s_len - old_len;
                self.durable_name_bytes
                    .copy_within(base + old_len..base + s_len, base + new_len);
                self.durable_name_bytes[base..base + new_len]
                    .copy_from_slice(&self.scratch_b[..new_len]);
                self.durable_names[k].len = (new_len + tail) as u16;
            }
        }
    }
}
