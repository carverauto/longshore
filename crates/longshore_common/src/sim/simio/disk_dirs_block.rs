// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Causal ordering and descendant-blocking checks for [`SimDisk`] directory
//! log entries.

use super::super::{SimDisk, SimName};
use super::DirChangeKind;

fn is_path_prefix(ancestor: &[u8], path: &[u8]) -> bool {
    if ancestor == b"/" {
        return true;
    }
    path.len() > ancestor.len()
        && path[..ancestor.len()] == *ancestor
        && path[ancestor.len()] == b'/'
}

fn inode_is_descendant_in_table(
    names: &[SimName],
    name_bytes: &[u8],
    max_name_len: usize,
    ancestor_ii: u32,
    node_ii: u32,
) -> bool {
    let Some(a_ni) = (0..names.len()).find(|&i| names[i].used && names[i].inode == ancestor_ii)
    else {
        return false;
    };
    let Some(n_ni) = (0..names.len()).find(|&i| names[i].used && names[i].inode == node_ii) else {
        return false;
    };
    let a_base = a_ni * max_name_len;
    let n_base = n_ni * max_name_len;
    let a_path = &name_bytes[a_base..a_base + names[a_ni].len as usize];
    let n_path = &name_bytes[n_base..n_base + names[n_ni].len as usize];
    is_path_prefix(a_path, n_path)
}

impl SimDisk {
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
        for _ in 0..=j {
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
            let Some(p) = parent else {
                return inode_is_descendant_in_table(
                    &self.durable_names,
                    &self.durable_name_bytes,
                    self.max_name_len,
                    ancestor_ii,
                    cur,
                );
            };
            cur = p;
        }
        false
    }
}
