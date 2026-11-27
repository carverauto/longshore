// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Opens, closes and path operations of a run, and power loss. An
//! operation that changes names waits until nothing is in flight, so that
//! the model knows what every name is when it judges; data operations of
//! open files run in any number at once between them. After a power loss
//! every name the model was sure of is opened and its promised bytes are
//! read back (F8), and the run goes on with names of a new epoch.

use super::run::Run;
use super::state::{HState, Room, Target, What};
use super::world::FILES;
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::OpenFlags;
use longshore_file::FileError;

impl Run {
    fn flags(&mut self) -> OpenFlags {
        let (read, write) = match self.below(3) {
            0 => (true, false),
            1 => (false, true),
            _ => (true, true),
        };
        let create = self.chance(700_000);
        OpenFlags::NONE
            .with_read(read)
            .with_write(write)
            .with_create(create)
            .with_exclusive(create && self.chance(100_000))
            .with_empty(write && self.chance(150_000))
            .with_dsync(self.chance(150_000))
            .with_direct(self.chance(150_000))
    }

    /// Open a name (or, now and then, a directory).
    pub fn open(&mut self) {
        let which = self.below(10);
        let dir = OpenFlags::NONE.with_read(true).with_directory(true);
        self.settle();
        let (path, target, at_dir, flags, expect) = if which < 8 {
            let n = self.below(self.st.names.len() as u64) as usize;
            let flags = self.flags();
            let expect = match (self.st.reachable(n), self.st.names[n].binding) {
                (false, _) => Err(FileError::NotFound),
                (true, Some(_)) if flags.create() && flags.exclusive() => Err(FileError::Exists),
                (true, Some(_)) => Ok(()),
                (true, None) if flags.create() => Ok(()),
                (true, None) => Err(FileError::NotFound),
            };
            (self.st.names[n].path.clone(), Some(n), None, flags, expect)
        } else if which == 8 {
            ("/".to_string(), None, Some(Target::Root), dir, Ok(()))
        } else {
            let made = if self.st.sub.made {
                Ok(())
            } else {
                Err(FileError::NotFound)
            };
            (self.st.sub.path.clone(), None, Some(Target::Sub), dir, made)
        };
        let full = self.st.handles.len() as u32 >= FILES;
        let room = if full { Room::Never } else { Room::Must };
        let t = self.ask("open", &[], room, |s, io, _| s.open(io, &path, flags));
        if let Some(t) = t {
            // Emptying a file is a change of every byte, in flight with
            // what comes after it.
            let bound = target.and_then(|n| self.st.bound(n));
            if let (Some(i), true, Ok(())) = (bound, flags.empty(), expect) {
                self.begin_change(i, 0, super::book::SPAN as u64, None);
                // begin_change took it as a change; it is an emptying one.
                let ino = &mut self.st.inodes[i];
                ino.end(0, super::book::SPAN, |_| Some(None), None, false);
                ino.emptying();
            }
            let w = What::Open {
                target,
                dir: at_dir,
                flags,
                expect,
            };
            self.pend(t, None, w, Vec::new());
        }
    }

    /// Close a handle, with its operations in flight or not.
    pub fn close(&mut self) {
        let Some(f) = self.pick(false) else {
            return self.turn();
        };
        let why = match self.st.handle(f).map(|h| h.state) {
            None => vec![FileError::Stale],
            Some(HState::Closing) => vec![FileError::Closing],
            Some(HState::Open | HState::Poisoned(_)) => Vec::new(),
        };
        let t = self.ask("close", &why, Room::Must, |s, io, _| s.close(io, f));
        if let Some(t) = t {
            if self.st.in_flight(f) > 0 {
                self.stats.closes_held += 1;
            }
            if let Some(h) = self.st.handle_mut(f) {
                h.state = HState::Closing;
            }
            self.pend(t, Some(f), What::Close, Vec::new());
        }
    }

    /// Whether an open or a path operation is in flight: then what a name
    /// is cannot be said.
    pub fn names_moving(&self) -> bool {
        self.st.pending.iter().any(|p| {
            matches!(
                p.what,
                What::Open { .. } | What::Rename { .. } | What::Unlink { .. } | What::Mkdir { .. }
            )
        })
    }

    /// What a status of name `n` must report, where it can be said.
    pub fn status_of(&self, n: usize) -> (Option<Result<u64, FileError>>, Option<usize>) {
        if self.names_moving() {
            return (None, None);
        }
        match self.st.bound(n) {
            None => (Some(Err(FileError::NotFound)), None),
            Some(i) => {
                let ino = &self.st.inodes[i];
                let sure = ino.size_busy == 0 && self.campaign.checks_bytes();
                let size = ino.size.filter(|_| sure);
                (size.map(Ok), Some(i))
            }
        }
    }

    pub fn status(&mut self) {
        let n = self.below(self.st.names.len() as u64) as usize;
        let path = self.st.names[n].path.clone();
        let (expect, inode) = self.status_of(n);
        let room = self.st.room(&[], 1);
        if let Some(t) = self.ask("status", &[], room, |s, io, _| s.status(io, &path)) {
            self.pend(t, None, What::Status { expect, inode }, Vec::new());
        }
    }

    /// Rename (0), unlink (1) or make the directory (2).
    pub fn path_op(&mut self, which: u32) {
        self.settle();
        let len = self.st.names.len() as u64;
        let (n, m) = (self.below(len) as usize, self.below(len) as usize);
        if which == 0 && n == m {
            return;
        }
        let missing = Err(FileError::NotFound);
        let (w, paths) = match which {
            0 => {
                let ok = self.st.bound(n).is_some() && self.st.reachable(m);
                let expect = if ok { Ok(0) } else { missing };
                let w = What::Rename {
                    from: n,
                    to: m,
                    expect,
                };
                (
                    w,
                    [self.st.names[n].path.clone(), self.st.names[m].path.clone()],
                )
            }
            1 => {
                let expect = if self.st.bound(n).is_some() {
                    Ok(0)
                } else {
                    missing
                };
                (
                    What::Unlink { name: n, expect },
                    [self.st.names[n].path.clone(), String::new()],
                )
            }
            _ => {
                let expect = if self.st.sub.made {
                    Err(FileError::Exists)
                } else {
                    Ok(0)
                };
                (
                    What::Mkdir { expect },
                    [self.st.sub.path.clone(), String::new()],
                )
            }
        };
        let room = self.st.room(&[], 1);
        let [a, b] = &paths;
        let t = match which {
            0 => self.ask("rename", &[], room, |s, io, _| s.rename(io, a, b)),
            1 => self.ask("unlink", &[], room, |s, io, _| s.unlink(io, a)),
            _ => self.ask("mkdir", &[], room, |s, io, _| s.mkdir(io, a)),
        };
        if let Some(t) = t {
            self.pend(t, None, w, Vec::new());
        }
    }

    /// A rename's, unlink's or mkdir's event: the names follow.
    pub fn path_done(&mut self, w: &What, result: Result<u64, FileError>, now: u64) {
        let st = &mut self.st;
        let moved = |names: &mut Vec<super::state::Name>, n: usize, to: Option<usize>, dirs| {
            names[n].binding = to;
            names[n].change(now, dirs);
        };
        match *w {
            What::Rename { from, to, expect } => {
                assert_eq!(result, expect, "rename");
                if result.is_ok() {
                    let (a, b) = (&st.names[from], &st.names[to]);
                    let both = [!a.in_sub || !b.in_sub, a.in_sub || b.in_sub];
                    let (bound, inherited, via) = (a.binding, a.need, a.via_sub);
                    moved(&mut st.names, to, bound, both);
                    let t = &mut st.names[to];
                    t.need = [t.need[0].max(inherited[0]), t.need[1].max(inherited[1])];
                    t.via_sub |= via;
                    let own = [!st.names[from].in_sub, st.names[from].in_sub];
                    moved(&mut st.names, from, None, own);
                }
            }
            What::Unlink { name, expect } => {
                assert_eq!(result, expect, "unlink");
                if result.is_ok() {
                    let own = [!st.names[name].in_sub, st.names[name].in_sub];
                    moved(&mut st.names, name, None, own);
                }
            }
            What::Mkdir { expect } => {
                assert_eq!(result, expect, "mkdir");
                if result.is_ok() {
                    st.sub.made = true;
                    st.sub.changed = now;
                }
            }
            What::Open { .. }
            | What::Close
            | What::Read { .. }
            | What::Write { .. }
            | What::Sync { .. }
            | What::Reserve { .. }
            | What::Punch { .. }
            | What::Status { .. } => panic!("not a path operation"),
        }
    }

    /// Sync a directory handle, opening one when there is none.
    pub fn dir_sync(&mut self) {
        let dirs: Vec<_> = self
            .st
            .handles
            .iter()
            .filter(|h| matches!(h.target, Target::Root | Target::Sub))
            .map(|h| h.file)
            .collect();
        let Some(f) = dirs.first().copied() else {
            return self.open();
        };
        let target = self.st.handle(f).map(|h| h.target);
        let (why, room) = self.refusals(f, super::ops::Need::Any);
        let t = self.ask("dir sync", &why, room, |s, io, _| s.sync(io, f, false));
        if let (Some(t), Some(target)) = (t, target) {
            self.pend(t, Some(f), What::Sync { target }, Vec::new());
        }
    }

    /// The machine loses power: the set is told, every ticket in flight
    /// ends without an event and every hold of the set is let go (C12);
    /// what the model was sure of is read back (F8).
    pub fn power_loss(&mut self) {
        self.note(format_args!("power loss"));
        self.rig.io().set_room(None);
        self.squeeze = 0;
        sealed(|| {
            self.rig.io().inner_mut().power_loss();
            self.rig.io().lost();
            self.rig.call(|s, _, x| s.power_lost(x));
        });
        for p in &self.st.pending {
            for x in &p.extents {
                let held = self.rig.x().holds(*x).map_or(0, u16::from);
                assert_eq!(held, self.st.ours(*x), "C12: the set let go of {x:?}");
            }
        }
        self.st.pending.clear();
        self.pages_busy = [false; super::world::PAGES as usize];
        for (e, n) in self.st.ours.clone() {
            for _ in 0..n {
                self.release(e);
            }
        }
        let u = self.rig.usage();
        assert_eq!((u.files(), u.ops(), u.waiting()), (0, 0, 0), "C12: {u:?}");
        for h in core::mem::take(&mut self.st.handles) {
            self.st.last_gen[h.file.slot() as usize] = Some(h.file.generation());
            self.st.stale.push(h.file);
        }
        self.verify_kept();
        self.mark = self.rig.io().submitted();
        self.st.new_epoch();
        self.stats.power_losses += 1;
    }

    /// F8: every name the model was sure of names a file that holds every
    /// byte it was promised.
    fn verify_kept(&mut self) {
        if !self.campaign.checks_bytes() {
            return;
        }
        let ro = OpenFlags::NONE.with_read(true);
        for n in 0..self.st.names.len() {
            let name = &self.st.names[n];
            let Some(i) = name.sure.filter(|_| !name.via_sub || self.st.sub.sure) else {
                continue;
            };
            let path = name.path.clone();
            let f = self.rig.try_open(&path, ro);
            let f = f.unwrap_or_else(|e| panic!("F8: {path} is gone after a power loss: {e:?}"));
            let end = self.st.inodes[i].promised_end();
            let mut off = 0;
            while off < end {
                let len = (end - off).min(super::world::EXTENT as usize);
                let (r, got) = self.rig.read(f, off as u64, len as u32);
                let r = r.unwrap_or_else(|e| panic!("F8: reading {path} back: {e:?}"));
                for k in 0..len {
                    if let Some(v) = self.st.inodes[i].durable[off + k] {
                        assert!((k as u64) < r, "F8: {path} ends before byte {}", off + k);
                        assert_eq!(got[k], v, "F8: byte {} of {path}", off + k);
                        self.stats.kept_bytes += 1;
                    }
                }
                off += len;
            }
            assert_eq!(self.rig.close(f), Ok(0));
            self.st.last_gen[f.slot() as usize] = Some(f.generation());
        }
    }

    /// An operation of a handle closed or lost: `Stale`.
    pub fn use_stale(&mut self) {
        if self.st.stale.is_empty() {
            return self.turn();
        }
        let i = self.below(self.st.stale.len() as u64) as usize;
        let f = self.st.stale[i];
        if self.st.handle(f).is_some() {
            return;
        }
        let stale = [FileError::Stale];
        let t = match self.below(3) {
            0 => self.ask("stale sync", &stale, Room::Must, |s, io, _| {
                s.sync(io, f, true)
            }),
            1 => self.ask("stale close", &stale, Room::Must, |s, io, _| s.close(io, f)),
            _ => self.ask("stale punch", &stale, Room::Must, |s, io, _| {
                s.punch(io, f, (0, 4096))
            }),
        };
        assert!(t.is_none());
    }
}
