// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ordered sequences of a run (C7): writes, reads, syncs and statuses of
//! up to two files, in one call. A read after a write of the same file in
//! the sequence must see it (F5); a sequence is taken only if every step
//! has room.

use super::ops::Need;
use super::run::Run;
use super::state::{Pending, Room, Seq, Target, What};
use super::world::STEPS_MAX;
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::extents::Extent;
use longshore_file::{File, FileError, Step, Ticket};

/// A step as the run plans it, before it borrows anything.
enum Plan {
    Write(File, Extent, u32, u32, u64, Vec<u8>),
    Read(File, Extent, u32, u32, u64),
    Sync(File, bool),
    Status(usize),
}

impl Run {
    fn plan_step(&mut self, files: &[File], why: &mut Vec<FileError>) -> Option<Plan> {
        let f = files[self.below(files.len() as u64) as usize];
        let direct = self.st.handle(f).is_some_and(|h| h.flags.direct());
        let kind = self.below(4);
        if kind == 3 {
            return Some(Plan::Status(self.below(self.st.names.len() as u64) as usize));
        }
        if kind == 2 {
            why.extend(self.refusals(f, Need::Any).0);
            return Some(Plan::Sync(f, self.chance(500_000)));
        }
        let (off, at, len, bad) = self.shape(direct);
        let e = self.take()?;
        why.extend(bad);
        if kind == 0 {
            why.extend(self.refusals(f, Need::Write).0);
            let data =
                self.data(len.min(super::world::EXTENT - at.min(super::world::EXTENT)) as usize);
            self.rig.fill(e, at.min(super::world::EXTENT), &data);
            Some(Plan::Write(f, e, at, len, off, data))
        } else {
            why.extend(self.refusals(f, Need::Read).0);
            Some(Plan::Read(f, e, at, len, off))
        }
    }

    pub fn sequence(&mut self) {
        let mut files: Vec<File> = Vec::new();
        for _ in 0..2 {
            if let Some(f) = self.pick(true) {
                files.push(f);
            }
        }
        if files.is_empty() {
            return self.turn();
        }
        let n = if self.chance(30_000) {
            STEPS_MAX + 1
        } else {
            2 + self.below(u64::from(STEPS_MAX) - 1) as u32
        };
        let mut why = Vec::new();
        let mut plans = Vec::new();
        for _ in 0..n {
            match self.plan_step(&files, &mut why) {
                Some(p) => plans.push(p),
                None => break,
            }
        }
        if plans.len() as u32 > STEPS_MAX {
            why.push(FileError::TooLarge);
        }
        let paths: Vec<String> = plans
            .iter()
            .map(|p| match p {
                Plan::Status(name) => self.st.names[*name].path.clone(),
                Plan::Write(..) | Plan::Read(..) | Plan::Sync(..) => String::new(),
            })
            .collect();
        let steps: Vec<Step<'_>> = plans
            .iter()
            .zip(&paths)
            .map(|(p, path)| step(p, path))
            .collect();
        let of: Vec<File> = plans.iter().filter_map(owner).collect();
        let statuses = plans
            .iter()
            .filter(|p| matches!(p, Plan::Status(_)))
            .count() as u32;
        let room = self.st.room(&of, statuses);
        let first_is_file_op = !matches!(plans.first(), Some(Plan::Status(_)) | None);
        self.maybe_place(why.is_empty() && room == Room::Must && first_is_file_op);
        let mut ts = [Ticket::NONE; STEPS_MAX as usize + 1];
        let before = (self.rig.usage(), self.rig.io().submitted());
        let got = sealed(|| self.rig.call(|s, io, x| s.sequence(io, x, &steps, &mut ts)));
        self.judge("sequence", got.map(|_| ()), &why, room, before);
        drop(steps);
        match got {
            Ok(k) => {
                assert_eq!(k, plans.len(), "a ticket for every step");
                self.taken(plans, &ts);
            }
            Err(_) => {
                for p in plans {
                    if let Plan::Write(_, e, ..) | Plan::Read(_, e, ..) = p {
                        self.release(e);
                    }
                }
            }
        }
    }

    /// The records of a sequence's reads: judged against the bytes before
    /// the sequence, with its earlier writes laid over them (F5).
    fn read_records(&self, plans: &[Plan]) -> Vec<Option<What>> {
        let mut records = Vec::new();
        for (k, p) in plans.iter().enumerate() {
            let Plan::Read(f, e, at, len, off) = *p else {
                records.push(None);
                continue;
            };
            let Some(Target::Inode(i)) = self.st.handle(f).map(|h| h.target) else {
                records.push(None);
                continue;
            };
            let mut w = self.read_record(i, off, len, e, at);
            if let What::Read { snap, sure, .. } = &mut w {
                for q in &plans[..k] {
                    let Plan::Write(g, _, _, _, woff, data) = q else {
                        continue;
                    };
                    if self.st.handle(*g).map(|h| h.target) != Some(Target::Inode(i)) {
                        continue;
                    }
                    for (j, v) in data.iter().enumerate() {
                        let b = *woff + j as u64;
                        if b >= off && b < off + snap.len() as u64 {
                            let at_b = (b - off) as usize;
                            snap[at_b] = *v;
                            sure[at_b] = self.st.inodes[i].taint[b as usize] == 0;
                        }
                    }
                }
            }
            records.push(Some(w));
        }
        records
    }

    /// The model's bookkeeping of a sequence taken: every step in flight,
    /// in order; a read sees the writes before it in the sequence.
    fn taken(&mut self, plans: Vec<Plan>, ts: &[Ticket]) {
        let id = self.st.seqs.len();
        self.st.seqs.push(Seq {
            reported: vec![false; plans.len()],
            failed_at: None,
        });
        self.stats.sequences += 1;
        let records = self.read_records(&plans);
        for (k, (p, read)) in plans.into_iter().zip(records).enumerate() {
            let (what, file, extents) = match p {
                Plan::Write(f, e, _, len, off, data) => {
                    let Some(Target::Inode(i)) = self.st.handle(f).map(|h| h.target) else {
                        continue;
                    };
                    let dsync = self.st.handle(f).is_some_and(|h| h.flags.dsync());
                    self.begin_change(i, off, off + u64::from(len), Some(id));
                    let w = What::Write {
                        inode: i,
                        off,
                        data,
                        dsync,
                        pages: 0,
                    };
                    (w, Some(f), vec![e])
                }
                Plan::Read(f, e, ..) => match read {
                    Some(w) => (w, Some(f), vec![e]),
                    None => continue,
                },
                Plan::Sync(f, _) => {
                    let Some(target) = self.st.handle(f).map(|h| h.target) else {
                        continue;
                    };
                    (What::Sync { target }, Some(f), Vec::new())
                }
                Plan::Status(n) => {
                    let (expect, _) = self.status_of(n);
                    let expect = expect.filter(|e| e.is_err());
                    (
                        What::Status {
                            expect,
                            inode: None,
                        },
                        None,
                        Vec::new(),
                    )
                }
            };
            self.pend_step(ts[k], (id, k), file, what, extents);
        }
    }

    /// Step `k` of sequence `id` in flight; the caller's holds on a
    /// write's extents let go of now or kept.
    fn pend_step(
        &mut self,
        t: Ticket,
        (id, k): (usize, usize),
        file: Option<File>,
        what: What,
        extents: Vec<Extent>,
    ) {
        let accepted = self.st.tick();
        let line = what.brief();
        self.note(format_args!(
            "  step {k} of {id}: {t:?} of {file:?}: {line}"
        ));
        let read = matches!(what, What::Read { .. });
        self.st.pending.push(Pending {
            ticket: t,
            file,
            what,
            accepted,
            extents: extents.clone(),
            seq: Some((id, k)),
        });
        for e in extents {
            if !read && self.chance(500_000) {
                self.release(e);
            }
        }
    }
}

fn step<'a>(p: &Plan, path: &'a str) -> Step<'a> {
    match *p {
        Plan::Write(file, extent, at, len, file_off, _) => Step::Write {
            file,
            extent,
            at,
            len,
            file_off,
        },
        Plan::Read(file, extent, at, len, file_off) => Step::Read {
            file,
            extent,
            at,
            len,
            file_off,
        },
        Plan::Sync(file, data_only) => Step::Sync { file, data_only },
        Plan::Status(_) => Step::Status { path },
    }
}

fn owner(p: &Plan) -> Option<File> {
    match *p {
        Plan::Write(f, ..) | Plan::Read(f, ..) | Plan::Sync(f, _) => Some(f),
        Plan::Status(_) => None,
    }
}
