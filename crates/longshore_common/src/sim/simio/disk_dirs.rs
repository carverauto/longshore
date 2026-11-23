// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Preallocated unsynced directory change log, directory sync, and crash
//! settlement for [`SimDisk`].

use super::{Prng, SimDisk};

#[path = "disk_dirs_block.rs"]
mod block;

#[derive(Clone, Copy)]
pub(super) enum DirChangeKind {
    Empty,
    Create {
        parent: u32,
        child: u32,
        name_len: u16,
    },
    Mkdir {
        parent: u32,
        child: u32,
        name_len: u16,
    },
    Unlink {
        parent: u32,
        child: u32,
        name_len: u16,
    },
    Rename {
        old_parent: u32,
        new_parent: u32,
        moved: u32,
        replaced: Option<u32>,
        old_len: u16,
        new_len: u16,
    },
}

impl DirChangeKind {
    fn refs(self) -> [Option<u32>; 4] {
        match self {
            Self::Empty => [None, None, None, None],
            Self::Create { parent, child, .. }
            | Self::Mkdir { parent, child, .. }
            | Self::Unlink { parent, child, .. } => [Some(parent), Some(child), None, None],
            Self::Rename {
                old_parent,
                new_parent,
                moved,
                replaced,
                ..
            } => [Some(old_parent), Some(new_parent), Some(moved), replaced],
        }
    }

    fn parents(self) -> [Option<u32>; 2] {
        match self {
            Self::Empty => [None, None],
            Self::Create { parent, .. }
            | Self::Mkdir { parent, .. }
            | Self::Unlink { parent, .. } => [Some(parent), None],
            Self::Rename {
                old_parent,
                new_parent,
                ..
            } => [Some(old_parent), Some(new_parent)],
        }
    }

    fn subjects(self) -> [Option<u32>; 2] {
        match self {
            Self::Empty => [None, None],
            Self::Create { child, .. } | Self::Mkdir { child, .. } | Self::Unlink { child, .. } => {
                [Some(child), None]
            }
            Self::Rename {
                moved, replaced, ..
            } => [Some(moved), replaced],
        }
    }

    fn created_or_moved_dir(self, inodes: &[super::SimInode]) -> Option<u32> {
        match self {
            Self::Mkdir { child, .. } => Some(child),
            Self::Rename { moved, .. } if inodes[moved as usize].is_dir => Some(moved),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct DirChange {
    pub(super) kind: DirChangeKind,
    pub(super) arena_slot: u16,
    pub(super) old_dir_synced: bool,
    pub(super) new_dir_synced: bool,
    pub(super) poisoned: bool,
}

impl DirChange {
    pub(super) const EMPTY: Self = Self {
        kind: DirChangeKind::Empty,
        arena_slot: 0,
        old_dir_synced: false,
        new_dir_synced: false,
        poisoned: false,
    };
}

impl SimDisk {
    pub(super) fn record_dir_change(
        &mut self,
        kind: DirChangeKind,
        a_range: core::ops::Range<usize>,
        b_range: core::ops::Range<usize>,
    ) {
        if self.dir_log.is_empty() {
            return;
        }
        if self.dir_log_len >= self.dir_log.len() {
            self.evict_one_dir_change();
        }
        let slot = self
            .dir_arena_used
            .iter()
            .position(|&u| !u)
            .expect("dir_arena capacity matches dir_log");
        self.dir_arena_used[slot] = true;
        let base = slot * 2 * self.max_name_len;
        let a_len = a_range.len();
        self.dir_name_arena[base..base + a_len].copy_from_slice(&self.scratch_a[a_range]);
        let b_len = b_range.len();
        if b_len > 0 {
            let b_off = base + self.max_name_len;
            self.dir_name_arena[b_off..b_off + b_len].copy_from_slice(&self.scratch_b[b_range]);
        }
        for r in kind.refs().into_iter().flatten() {
            self.inodes[r as usize].dir_log_refs += 1;
        }
        let cross_dir = matches!(
            kind,
            DirChangeKind::Rename {
                old_parent,
                new_parent,
                ..
            } if old_parent != new_parent
        );
        self.dir_log[self.dir_log_len] = DirChange {
            kind,
            arena_slot: slot as u16,
            old_dir_synced: false,
            new_dir_synced: !cross_dir,
            poisoned: false,
        };
        self.dir_log_len += 1;
    }

    fn evict_one_dir_change(&mut self) {
        if let Some(idx) = (0..self.dir_log_len).find(|&i| {
            !self.dir_log[i].poisoned
                && !self.dir_entry_blocked_by_earlier(i)
                && self.dir_parents_durable(self.dir_log[i].kind)
        }) {
            let refs = self.dir_log[idx].kind.refs();
            let reclaim = self.apply_dir_change_to_durable(idx);
            self.remove_dir_entry(idx);
            self.reclaim_dir_inodes(refs, reclaim);
        } else if self.dir_log_len > 0 {
            let refs = self.dir_log[0].kind.refs();
            self.remove_dir_entry(0);
            for r in refs.into_iter().flatten() {
                self.free_inode_if_dead(r as usize);
            }
        }
    }

    fn dir_parents_durable(&self, kind: DirChangeKind) -> bool {
        match kind {
            DirChangeKind::Empty => false,
            DirChangeKind::Create { parent, .. }
            | DirChangeKind::Mkdir { parent, .. }
            | DirChangeKind::Unlink { parent, .. } => self.find_durable_inode(parent).is_some(),
            DirChangeKind::Rename {
                old_parent,
                new_parent,
                ..
            } => {
                self.find_durable_inode(old_parent).is_some()
                    && self.find_durable_inode(new_parent).is_some()
            }
        }
    }

    fn remove_dir_entry(&mut self, idx: usize) {
        let e = self.dir_log[idx];
        for r in e.kind.refs().into_iter().flatten() {
            let ii = r as usize;
            self.inodes[ii].dir_log_refs = self.inodes[ii].dir_log_refs.saturating_sub(1);
        }
        self.dir_arena_used[e.arena_slot as usize] = false;
        self.dir_log.copy_within(idx + 1..self.dir_log_len, idx);
        self.dir_log_len -= 1;
    }

    fn dir_entry_names(&self, idx: usize) -> [Option<(u32, &[u8])>; 2] {
        let e = self.dir_log[idx];
        let base_off = e.arena_slot as usize * 2 * self.max_name_len;
        match e.kind {
            DirChangeKind::Empty => [None, None],
            DirChangeKind::Create {
                parent, name_len, ..
            }
            | DirChangeKind::Mkdir {
                parent, name_len, ..
            }
            | DirChangeKind::Unlink {
                parent, name_len, ..
            } => [
                Some((
                    parent,
                    &self.dir_name_arena[base_off..base_off + name_len as usize],
                )),
                None,
            ],
            DirChangeKind::Rename {
                old_parent,
                new_parent,
                old_len,
                new_len,
                ..
            } => {
                let new_off = base_off + self.max_name_len;
                [
                    Some((
                        old_parent,
                        &self.dir_name_arena[base_off..base_off + old_len as usize],
                    )),
                    Some((
                        new_parent,
                        &self.dir_name_arena[new_off..new_off + new_len as usize],
                    )),
                ]
            }
        }
    }

    pub(super) fn sync_dir(&mut self, dir_ii: usize) {
        let d = dir_ii as u32;
        for entry in &mut self.dir_log[..self.dir_log_len] {
            if entry.poisoned {
                continue;
            }
            match entry.kind {
                DirChangeKind::Create { parent, .. }
                | DirChangeKind::Mkdir { parent, .. }
                | DirChangeKind::Unlink { parent, .. }
                    if parent == d =>
                {
                    entry.old_dir_synced = true;
                }
                DirChangeKind::Rename {
                    old_parent,
                    new_parent,
                    ..
                } => {
                    if old_parent == d {
                        entry.old_dir_synced = true;
                    }
                    if new_parent == d {
                        entry.new_dir_synced = true;
                    }
                }
                _ => {}
            }
        }
        let mut i = 0;
        while i < self.dir_log_len {
            let e = self.dir_log[i];
            if !e.poisoned
                && e.old_dir_synced
                && e.new_dir_synced
                && !self.dir_entry_blocked_by_earlier(i)
                && self.dir_parents_durable(e.kind)
            {
                let refs = e.kind.refs();
                let reclaim = self.apply_dir_change_to_durable(i);
                self.remove_dir_entry(i);
                self.reclaim_dir_inodes(refs, reclaim);
            } else {
                i += 1;
            }
        }
    }

    pub(super) fn poison_dir(&mut self, dir_ii: usize) {
        let d = dir_ii as u32;
        for entry in &mut self.dir_log[..self.dir_log_len] {
            if entry.poisoned {
                continue;
            }
            match entry.kind {
                DirChangeKind::Create { parent, .. }
                | DirChangeKind::Mkdir { parent, .. }
                | DirChangeKind::Unlink { parent, .. }
                    if parent == d && !entry.old_dir_synced =>
                {
                    entry.poisoned = true;
                }
                DirChangeKind::Rename {
                    old_parent,
                    new_parent,
                    ..
                } if (old_parent == d && !entry.old_dir_synced)
                    || (new_parent == d && !entry.new_dir_synced) =>
                {
                    entry.poisoned = true;
                }
                _ => {}
            }
        }
    }

    pub(super) fn settle_dir_log(&mut self, rng: &mut Prng) {
        for i in 0..self.dir_log_len {
            let e = self.dir_log[i];
            if e.poisoned {
                continue;
            }
            let keep = (e.old_dir_synced && e.new_dir_synced) || rng.below(2) == 1;
            if keep {
                self.apply_dir_change_to_durable(i);
            }
        }
        self.dir_log_len = 0;
        self.dir_arena_used.fill(false);
        for ino in self.inodes.iter_mut() {
            ino.dir_log_refs = 0;
        }
    }
}
