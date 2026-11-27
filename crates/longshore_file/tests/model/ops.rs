// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The data operations of a run: read, write, gathered write, sync,
//! reserve, punch; most well formed, some off a block, past an extent or
//! of a handle that cannot take them, each judged at the call.

use super::book::SPAN;
use super::run::Run;
use super::state::{HState, Pending, Room, Target, What};
use super::world::{BLOCK, EXTENT, SPANS_MAX};
use longshore_common::io::extents::Extent;
use longshore_common::io::Io;
use longshore_file::{File, FileError, Span};

/// A gathered write's spans (with the page each is of), extents, bytes,
/// and the refusal it was made to earn.
type Gathered = (
    Vec<(Span, Option<u32>)>,
    Vec<Extent>,
    Vec<u8>,
    Option<FileError>,
);

/// What an operation needs of its file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Read,
    Write,
    Any,
}

impl Run {
    /// A handle to use: mostly a live one of a file, now and then a stale
    /// one.
    pub fn pick(&mut self, files_only: bool) -> Option<File> {
        if !self.st.stale.is_empty() && self.chance(30_000) {
            let i = self.below(self.st.stale.len() as u64) as usize;
            return Some(self.st.stale[i]);
        }
        let live: Vec<File> = self
            .st
            .handles
            .iter()
            .filter(|h| !files_only || matches!(h.target, Target::Inode(_)))
            .map(|h| h.file)
            .collect();
        if live.is_empty() {
            return None;
        }
        Some(live[self.below(live.len() as u64) as usize])
    }

    /// What a call on `f` needing `need` must be refused with, besides
    /// `Full`, and the room for it.
    pub fn refusals(&self, f: File, need: Need) -> (Vec<FileError>, Room) {
        let Some(h) = self.st.handle(f) else {
            return (vec![FileError::Stale], Room::Must);
        };
        let mut why = Vec::new();
        if h.state == HState::Closing {
            why.push(FileError::Closing);
        }
        if h.poisoned && need != Need::Read {
            why.push(FileError::Poisoned);
        }
        let can = match need {
            Need::Read => h.flags.read(),
            Need::Write => h.flags.write(),
            Need::Any => true,
        };
        if !can {
            why.push(FileError::NotOpen);
        }
        (why, self.st.room(&[f], 0))
    }

    /// The inode `f` is of, and whether it is open for direct I/O.
    pub fn of(&self, f: File) -> (Option<usize>, bool, bool) {
        self.st
            .handle(f)
            .map_or((None, false, false), |h| match h.target {
                Target::Inode(i) => (Some(i), h.flags.direct(), h.flags.dsync()),
                Target::Root | Target::Sub => (None, false, false),
            })
    }

    /// A transfer's shape: file offset, place in the extent, length; and
    /// the refusal it was made to earn, if any.
    pub fn shape(&mut self, direct: bool) -> (u64, u32, u32, Option<FileError>) {
        if direct {
            let off = u64::from(BLOCK) * self.below(5);
            let len = BLOCK * (1 + self.below(2) as u32);
            let at = if len == BLOCK {
                BLOCK * self.below(2) as u32
            } else {
                0
            };
            if self.chance(50_000) {
                return (off + 512, at, len, Some(FileError::Unaligned));
            }
            return (off, at, len, None);
        }
        let off = self.below(24 * 1024);
        let len = 1 + self.below(u64::from(EXTENT)) as u32;
        if self.chance(30_000) {
            return (off, EXTENT - len / 2, len, Some(FileError::TooLarge));
        }
        let at = self.below(u64::from(EXTENT - len) + 1) as u32;
        (off, at, len, None)
    }

    /// The model's bookkeeping of a write-like operation taken: in flight
    /// over `[lo, hi)` of `inode`, so reads and statuses in flight there
    /// cannot be judged on those bytes, except a sequence's own earlier
    /// steps.
    pub fn begin_change(&mut self, inode: usize, lo: u64, hi: u64, seq: Option<usize>) {
        self.st.inodes[inode].begin(lo as usize, hi as usize);
        for p in &mut self.st.pending {
            let same_seq = seq.is_some() && p.seq.map(|s| s.0) == seq;
            if let What::Read {
                inode: i,
                off,
                sure,
                size_stable,
                ..
            } = &mut p.what
            {
                if *i == inode && !same_seq {
                    *size_stable = false;
                    for (k, s) in sure.iter_mut().enumerate() {
                        let b = *off + k as u64;
                        if b >= lo && b < hi {
                            *s = false;
                        }
                    }
                }
            } else if let What::Status {
                inode: Some(i),
                expect,
            } = &mut p.what
            {
                if *i == inode {
                    *expect = None;
                }
            }
        }
    }

    pub fn pend(&mut self, t: longshore_file::Ticket, f: Option<File>, what: What, x: Vec<Extent>) {
        let accepted = self.st.tick();
        self.note(format_args!(
            "  {t:?} of {f:?} at {accepted}: {}",
            what.brief()
        ));
        self.st.pending.push(Pending {
            ticket: t,
            file: f,
            what,
            accepted,
            extents: x,
            seq: None,
        });
    }

    /// Let go of the caller's hold now, or keep it until the event.
    fn maybe_release(&mut self, e: Extent, taken: bool) {
        if !taken || self.chance(500_000) {
            self.release(e);
        }
    }

    pub fn write(&mut self) {
        let Some(f) = self.pick(true) else {
            return self.turn();
        };
        let (inode, direct, dsync) = self.of(f);
        let (off, at, len, bad) = self.shape(direct);
        let Some(e) = self.take() else {
            return self.turn();
        };
        let data = self.data(len.min(EXTENT - at.min(EXTENT)) as usize);
        self.rig.fill(e, at.min(EXTENT), &data);
        let (mut why, room) = self.refusals(f, Need::Write);
        why.extend(bad);
        self.maybe_place(why.is_empty() && room == Room::Must);
        let t = self.ask("write_at", &why, room, |s, io, x| {
            s.write_at(io, x, f, (e, at, len), off)
        });
        if let (Some(t), Some(i)) = (t, inode) {
            self.begin_change(i, off, off + u64::from(len), None);
            let w = What::Write {
                inode: i,
                off,
                data,
                dsync,
                pages: 0,
            };
            self.pend(t, Some(f), w, vec![e]);
        }
        self.maybe_release(e, t.is_some());
    }

    /// The spans of a gathered write of `n` spans: of extents (each one
    /// taken, filled and held), or of pages of the receive area not in use;
    /// the bytes they hold in order, and the refusal they earn.
    fn gather(&mut self, n: u32, direct: bool) -> Gathered {
        let mut spans = Vec::new();
        let mut extents = Vec::new();
        let mut data = Vec::new();
        let mut bad = None;
        let mut used = self.pages_busy;
        for _ in 0..n {
            let page = (0..super::world::PAGES).find(|p| !used[*p as usize]);
            if let (Some(p), false, true) = (page, direct, self.chance(300_000)) {
                let len = 1 + self.below(1500) as u32;
                let off = self.below(u64::from(4096 - len)) as u32;
                let bytes = self.data(len as usize);
                let id = self.rig.limits().page(p);
                self.rig.io().buf(id)[off as usize..(off + len) as usize].copy_from_slice(&bytes);
                data.extend_from_slice(&bytes);
                used[p as usize] = true;
                spans.push((Span::Page { page: id, off, len }, Some(p)));
                continue;
            }
            let Some(e) = self.take() else { break };
            let (at, len) = if direct {
                if self.chance(30_000) {
                    bad = bad.or(Some(FileError::Unaligned));
                    (512, BLOCK)
                } else {
                    (BLOCK * self.below(2) as u32, BLOCK)
                }
            } else {
                let len = 1 + self.below(2000) as u32;
                (self.below(u64::from(EXTENT - len)) as u32, len)
            };
            let bytes = self.data(len as usize);
            self.rig.fill(e, at, &bytes);
            data.extend_from_slice(&bytes);
            extents.push(e);
            spans.push((Span::Extent { extent: e, at, len }, None));
        }
        (spans, extents, data, bad)
    }

    pub fn write_spans(&mut self) {
        let Some(f) = self.pick(true) else {
            return self.turn();
        };
        let (inode, direct, dsync) = self.of(f);
        let n = if self.chance(30_000) {
            SPANS_MAX + 1
        } else {
            1 + self.below(u64::from(SPANS_MAX)) as u32
        };
        let (spans, extents, data, bad) = self.gather(n, direct);
        let off = if direct {
            u64::from(BLOCK) * self.below(3)
        } else {
            self.below(16 * 1024)
        };
        let list: Vec<Span> = spans.iter().map(|(s, _)| *s).collect();
        let (mut why, room) = self.refusals(f, Need::Write);
        why.extend(bad);
        if list.len() > SPANS_MAX as usize {
            why.push(FileError::TooLarge);
        }
        if list.is_empty() {
            for e in extents {
                self.release(e);
            }
            return;
        }
        self.maybe_place(why.is_empty() && room == Room::Must);
        let t = self.ask("write_spans", &why, room, |s, io, x| {
            s.write_spans(io, x, f, &list, off)
        });
        if let (Some(t), Some(i)) = (t, inode) {
            let mut pages = 0;
            for p in spans.iter().filter_map(|(_, p)| *p) {
                self.pages_busy[p as usize] = true;
                pages |= 1 << p;
            }
            self.begin_change(i, off, off + data.len() as u64, None);
            let w = What::Write {
                inode: i,
                off,
                data,
                dsync,
                pages,
            };
            self.pend(t, Some(f), w, extents.clone());
        }
        for e in extents {
            self.maybe_release(e, t.is_some());
        }
    }

    /// A read's record: what the bytes must be where they can be judged.
    pub fn read_record(&self, inode: usize, off: u64, len: u32, e: Extent, at: u32) -> What {
        let n = (len as usize).min(SPAN.saturating_sub(off as usize));
        let ino = &self.st.inodes[inode];
        let r = off as usize..off as usize + n;
        let snap = ino.content[r.clone()].to_vec();
        let sure = r.map(|b| ino.known[b] && ino.taint[b] == 0).collect();
        let size_stable = ino.size.is_some() && ino.size_busy == 0;
        What::Read {
            inode,
            off,
            len,
            extent: e,
            at,
            snap,
            sure,
            size_stable,
        }
    }

    pub fn read(&mut self) {
        let Some(f) = self.pick(true) else {
            return self.turn();
        };
        let (inode, direct, _) = self.of(f);
        let (off, at, len, bad) = self.shape(direct);
        let Some(e) = self.take() else {
            return self.turn();
        };
        let (mut why, room) = self.refusals(f, Need::Read);
        why.extend(bad);
        self.maybe_place(why.is_empty() && room == Room::Must);
        let t = self.ask("read_at", &why, room, |s, io, x| {
            s.read_at(io, x, f, (e, at, len), off)
        });
        match (t, inode) {
            (Some(t), Some(i)) => {
                let w = self.read_record(i, off, len, e, at);
                self.pend(t, Some(f), w, vec![e]);
            }
            _ => self.release(e),
        }
    }

    pub fn sync(&mut self) {
        let Some(f) = self.pick(false) else {
            return self.turn();
        };
        let target = self.st.handle(f).map(|h| h.target);
        let data_only = self.chance(500_000);
        let (why, room) = self.refusals(f, Need::Any);
        self.maybe_place(why.is_empty() && room == Room::Must);
        let t = self.ask("sync", &why, room, |s, io, _| s.sync(io, f, data_only));
        if let (Some(t), Some(target)) = (t, target) {
            self.pend(t, Some(f), What::Sync { target }, Vec::new());
        }
    }
}
