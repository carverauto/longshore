// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Causal ordering and descendant-blocking checks for [`SimDisk`] directory
//! log entries.

use super::super::SimDisk;
use super::DirChangeKind;

impl SimDisk {
    fn durable_parent_inode(&self, node_ii: u32) -> Option<u32> {
        let ni = (0..self.durable_names.len())
            .find(|&i| self.durable_names[i].used && self.durable_names[i].inode == node_ii)?;
        let base = ni * self.max_name_len;
        let path = &self.durable_name_bytes[base..base + self.durable_names[ni].len as usize];
        let parent_path = match path.iter().rposition(|&b| b == b'/') {
            Some(0) | None => b"/" as &[u8],
            Some(pos) => &path[..pos],
        };
        let p_ni = (0..self.durable_names.len()).find(|&i| {
            if !self.durable_names[i].used {
                return false;
            }
            let p_base = i * self.max_name_len;
            &self.durable_name_bytes[p_base..p_base + self.durable_names[i].len as usize]
                == parent_path
        })?;
        Some(self.durable_names[p_ni].inode)
    }
    pub(super) fn dir_entry_blocked_by_earlier(&self, idx: usize) -> bool {
        let kind = self.dir_log[idx].kind;
        let parents = kind.parents();
        let subjects = kind.subjects();
        let names = self.dir_entry_names(idx);
        for j in 0..idx {
            let ej = self.dir_log[j].kind;
            if let Some(d) = ej.created_or_moved_dir(&self.inodes) {
                if parents
                    .into_iter()
                    .flatten()
                    .any(|p| self.inode_is_same_or_descendant_at(j, d, p))
                {
                    return true;
                }
            }
            if let DirChangeKind::Rename {
                moved, replaced, ..
            } = kind
            {
                if self.inodes[moved as usize].is_dir {
                    let removed_parent = match ej {
                        DirChangeKind::Unlink { parent, .. } => Some(parent),
                        DirChangeKind::Rename { old_parent, .. } => Some(old_parent),
                        _ => None,
                    };
                    if let Some(p) = removed_parent {
                        if self.inode_is_same_or_descendant_at(j, moved, p) {
                            return true;
                        }
                    }
                }
                if let Some(dst_ii) = replaced {
                    if self.inodes[dst_ii as usize].is_dir
                        && ej
                            .parents()
                            .into_iter()
                            .flatten()
                            .any(|p| self.inode_is_same_or_descendant_at(j, dst_ii, p))
                    {
                        return true;
                    }
                }
            }
            let ej_subjects = ej.subjects();
            if subjects
                .into_iter()
                .flatten()
                .any(|s| ej_subjects.into_iter().flatten().any(|es| es == s))
            {
                return true;
            }
            let ej_names = self.dir_entry_names(j);
            if names
                .into_iter()
                .flatten()
                .any(|n| ej_names.into_iter().flatten().any(|en| en == n))
            {
                return true;
            }
        }
        false
    }

    fn inode_is_same_or_descendant_at(&self, j: usize, ancestor_ii: u32, node_ii: u32) -> bool {
        let mut cur = node_ii;
        for _ in 0..=self.inodes.len() {
            if cur == ancestor_ii {
                return true;
            }
            let mut parent = None;
            for k in (0..j).rev() {
                match self.dir_log[k].kind {
                    DirChangeKind::Mkdir {
                        parent: p, child, ..
                    } if child == cur => {
                        parent = Some(p);
                        break;
                    }
                    DirChangeKind::Rename {
                        new_parent: p,
                        moved,
                        ..
                    } if moved == cur => {
                        parent = Some(p);
                        break;
                    }
                    _ => {}
                }
            }
            if let Some(p) = parent {
                cur = p;
            } else {
                if cur == 0 {
                    return false;
                }
                let Some(p) = self.durable_parent_inode(cur) else {
                    return false;
                };
                cur = p;
            }
        }
        false
    }
}
