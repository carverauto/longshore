// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the model keeps besides bytes: names and directories, the handles
//! the set gave out and their states, every ticket accepted and not yet
//! reported with what it must report, sequences, and the caller's holds.

use super::book::Inode;
use super::world::{FILES, PATHS, PER_FILE, SHARED};
use longshore_common::io::extents::Extent;
use longshore_common::io::OpenFlags;
use longshore_file::{File, FileError, Ticket};

/// A name of the run's present epoch: in the root, or in its directory.
pub struct Name {
    pub path: String,
    pub in_sub: bool,
    pub binding: Option<usize>,
    /// The file a power loss must leave it naming (F8).
    pub sure: Option<usize>,
    /// The clock of the latest change of it that a sync of the root, and
    /// of the epoch's directory, must follow before it is sure: a rename
    /// is durable once both its directories are synced, as the simulated
    /// disk keeps it; 0 for none.
    pub need: [u64; 2],
    /// It was in the epoch's directory since it was last sure: that
    /// directory must be durable too.
    pub via_sub: bool,
}

impl Name {
    /// Changed at `now`, in the directories `dirs` (root, sub).
    pub fn change(&mut self, now: u64, dirs: [bool; 2]) {
        self.sure = None;
        for (need, d) in self.need.iter_mut().zip(dirs) {
            if d {
                *need = now;
            }
        }
        self.via_sub |= dirs[1];
    }

    /// The directory `d` (0 root, 1 sub) was synced by a sync accepted at
    /// `accepted`: once nothing more is needed, the name is sure.
    pub fn dir_synced(&mut self, d: usize, accepted: u64) {
        if self.need[d] != 0 && self.need[d] < accepted {
            self.need[d] = 0;
            if self.need == [0, 0] {
                self.sure = self.binding;
            }
        }
    }
}

/// The epoch's directory.
pub struct Sub {
    pub path: String,
    pub made: bool,
    pub sure: bool,
    pub changed: u64,
}

/// What an open handle is of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Inode(usize),
    Root,
    Sub,
}

/// A handle's state, as the events taken say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HState {
    Open,
    /// Since the step of that number.
    Poisoned(usize),
    Closing,
}

/// A handle the set gave out and has not freed.
pub struct Handle {
    pub file: File,
    pub target: Target,
    pub flags: OpenFlags,
    pub state: HState,
    /// A failed sync or durable write poisoned it, also while closing.
    pub poisoned: bool,
}

/// What an accepted operation is, and what it must report.
pub enum What {
    Open {
        target: Option<usize>,
        dir: Option<Target>,
        flags: OpenFlags,
        expect: Result<(), FileError>,
    },
    Close,
    Read {
        inode: usize,
        off: u64,
        len: u32,
        extent: Extent,
        at: u32,
        snap: Vec<u8>,
        sure: Vec<bool>,
        size_stable: bool,
    },
    Write {
        inode: usize,
        off: u64,
        data: Vec<u8>,
        dsync: bool,
        /// Pages of the receive area it is from, a bit each.
        pages: u8,
    },
    Sync {
        target: Target,
    },
    Reserve {
        inode: usize,
        off: u64,
        len: u64,
        keep: bool,
    },
    Punch {
        inode: usize,
        off: u64,
        len: u64,
    },
    Status {
        expect: Option<Result<u64, FileError>>,
        inode: Option<usize>,
    },
    Rename {
        from: usize,
        to: usize,
        expect: Result<u64, FileError>,
    },
    Unlink {
        name: usize,
        expect: Result<u64, FileError>,
    },
    Mkdir {
        expect: Result<u64, FileError>,
    },
}

impl What {
    /// A line for a trace.
    pub fn brief(&self) -> String {
        match self {
            What::Open {
                target,
                dir,
                flags,
                expect,
            } => {
                format!("open {target:?}{dir:?} {flags:?} -> {expect:?}")
            }
            What::Close => "close".into(),
            What::Read {
                inode, off, len, ..
            } => format!("read i{inode} {off}+{len}"),
            What::Write {
                inode,
                off,
                data,
                dsync,
                ..
            } => {
                format!("write i{inode} {off}+{} dsync {dsync}", data.len())
            }
            What::Sync { target } => format!("sync {target:?}"),
            What::Reserve {
                inode,
                off,
                len,
                keep,
            } => format!("reserve i{inode} {off}+{len} {keep}"),
            What::Punch { inode, off, len } => format!("punch i{inode} {off}+{len}"),
            What::Status { expect, inode } => format!("status i{inode:?} -> {expect:?}"),
            What::Rename { from, to, expect } => format!("rename {from} {to} -> {expect:?}"),
            What::Unlink { name, expect } => format!("unlink {name} -> {expect:?}"),
            What::Mkdir { expect } => format!("mkdir -> {expect:?}"),
        }
    }
}

/// An accepted operation not yet reported.
pub struct Pending {
    pub ticket: Ticket,
    pub file: Option<File>,
    pub what: What,
    pub accepted: u64,
    pub extents: Vec<Extent>,
    /// Its sequence and its place in it.
    pub seq: Option<(usize, usize)>,
}

impl Pending {
    /// Whether it is an operation of a file (not an open, close or path
    /// operation): what takes a share of the file's slots.
    pub fn of_file(&self) -> bool {
        self.file.is_some() && !matches!(self.what, What::Open { .. } | What::Close)
    }

    /// Whether it takes a path slot.
    pub fn of_path(&self) -> bool {
        matches!(
            self.what,
            What::Status { .. } | What::Rename { .. } | What::Unlink { .. } | What::Mkdir { .. }
        )
    }
}

/// Whether there is room for an operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Room {
    /// There is: it must be taken.
    Must,
    /// There may be: `Full` or taken.
    Maybe,
    /// There is not: it must be `Full`.
    Never,
}

/// A sequence's progress.
pub struct Seq {
    /// Which steps have reported.
    pub reported: Vec<bool>,
    /// The first step that failed.
    pub failed_at: Option<usize>,
}

/// The model, without bytes' detail.
pub struct State {
    pub inodes: Vec<Inode>,
    pub names: Vec<Name>,
    pub sub: Sub,
    pub handles: Vec<Handle>,
    pub stale: Vec<File>,
    pub pending: Vec<Pending>,
    pub seqs: Vec<Seq>,
    /// The caller's holds on extents it took.
    pub ours: Vec<(Extent, u16)>,
    /// The generation each file slot's last handle had.
    pub last_gen: [Option<u32>; FILES as usize],
    pub clock: u64,
    pub epoch: u32,
}

impl State {
    pub fn new() -> State {
        let mut s = State {
            inodes: Vec::new(),
            names: Vec::new(),
            sub: Sub {
                path: String::new(),
                made: false,
                sure: false,
                changed: 0,
            },
            handles: Vec::new(),
            stale: Vec::new(),
            pending: Vec::new(),
            seqs: Vec::new(),
            ours: Vec::new(),
            last_gen: [None; FILES as usize],
            clock: 1,
            epoch: 0,
        };
        s.new_epoch();
        s
    }

    /// Names of a fresh epoch: two in the root, two in a directory not yet
    /// made.
    pub fn new_epoch(&mut self) {
        self.epoch += 1;
        let e = self.epoch;
        self.sub = Sub {
            path: format!("/e{e}d"),
            made: false,
            sure: false,
            changed: 0,
        };
        self.names = ["a", "b", "d/a", "d/b"]
            .iter()
            .map(|n| Name {
                path: format!("/e{e}{n}"),
                in_sub: n.starts_with('d'),
                binding: None,
                sure: None,
                need: [0, 0],
                via_sub: false,
            })
            .collect();
    }

    pub fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub fn handle(&self, f: File) -> Option<&Handle> {
        self.handles.iter().find(|h| h.file == f)
    }

    pub fn handle_mut(&mut self, f: File) -> Option<&mut Handle> {
        self.handles.iter_mut().find(|h| h.file == f)
    }

    /// Operations of `f` in flight, that take its share of slots.
    pub fn in_flight(&self, f: File) -> u32 {
        self.pending
            .iter()
            .filter(|p| p.of_file() && p.file == Some(f))
            .count() as u32
    }

    /// Whether one more operation of each file in `of` (counted together)
    /// and `paths` more path operations fit (C2). Which slot each
    /// operation in flight holds is the set's own, so the shared slots in
    /// use are known only between every file's operations beyond its own
    /// slot and all of them: `Maybe` is the room that lies between.
    pub fn room(&self, of: &[File], paths: u32) -> Room {
        let paths_used = self.pending.iter().filter(|p| p.of_path()).count() as u32;
        if paths_used + paths > PATHS {
            return Room::Never;
        }
        let all: u32 = self.handles.iter().map(|h| self.in_flight(h.file)).sum();
        let mut worst = all;
        let mut seen: Vec<File> = Vec::new();
        for f in of {
            if seen.contains(f) {
                continue;
            }
            seen.push(*f);
            let (n, k) = (
                self.in_flight(*f),
                of.iter().filter(|g| *g == f).count() as u32,
            );
            if n + k > PER_FILE {
                return Room::Never;
            }
            // A file with nothing in flight has its own slot free.
            worst += if n == 0 { k - 1 } else { k };
        }
        if worst <= SHARED {
            Room::Must
        } else {
            Room::Maybe
        }
    }

    /// The caller's holds on `e`.
    pub fn ours(&self, e: Extent) -> u16 {
        self.ours
            .iter()
            .find(|(x, _)| *x == e)
            .map_or(0, |(_, n)| *n)
    }

    /// The caller took or let go of a hold on `e`.
    pub fn hold(&mut self, e: Extent, more: bool) {
        match self.ours.iter().position(|(x, _)| *x == e) {
            Some(i) if more => self.ours[i].1 += 1,
            Some(i) => {
                self.ours[i].1 -= 1;
                if self.ours[i].1 == 0 {
                    let _gone: (Extent, u16) = self.ours.swap_remove(i);
                }
            }
            None if more => self.ours.push((e, 1)),
            None => panic!("letting go of {e:?}, never held"),
        }
    }

    /// Whether the name `n`'s directory exists.
    pub fn reachable(&self, n: usize) -> bool {
        !self.names[n].in_sub || self.sub.made
    }

    /// The inode `n` names, if it can be reached.
    pub fn bound(&self, n: usize) -> Option<usize> {
        if self.reachable(n) {
            self.names[n].binding
        } else {
            None
        }
    }
}
