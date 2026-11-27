// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A completion taken: what it means for its operation (contract C4, C5,
//! C6, C10), and the report that ends an operation.

use crate::errno::{ended, errno_of, error, sync_error, transfer_error};
use crate::plan::STATUS_AREA;
use crate::state::{FState, OState, PathKind, Work, NONE};
use crate::{ErrorKind, FileError, Files};
use longshore_common::io::extents::Extents;
use longshore_common::io::{file_size, Io};

/// What a completion leaves of its operation.
pub(crate) enum Outcome {
    /// It is over, with this result.
    Done(Result<u64, FileError>),
    /// A transfer with more to move: submit the rest.
    More,
}

impl Files {
    /// Completion `res` of record `i`.
    pub(crate) fn completed<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, i: u32, res: i32) {
        if self.recs[i as usize].state != OState::Submitted {
            // Nothing of the set is in flight under this token: a
            // completion from before `power_lost`, which a backend that
            // lost power does not deliver.
            return;
        }
        if self.recs[i as usize].lead != NONE {
            self.step_completed(io, x, i, res);
            return;
        }
        match self.outcome(io, x, i, res) {
            Outcome::Done(result) => self.finish(io, x, i, result),
            Outcome::More if self.blocked(i) => {
                let e = self.blocked_error(i);
                self.finish(io, x, i, Err(e));
            }
            Outcome::More => self.launch(io, i),
        }
    }

    /// What completion `res` means for record `i`.
    pub(crate) fn outcome<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        i: u32,
        res: i32,
    ) -> Outcome {
        let r = self.recs[i as usize];
        let plain = || {
            if res < 0 {
                Outcome::Done(Err(error(errno_of(res))))
            } else {
                Outcome::Done(Ok(0))
            }
        };
        match r.work {
            Work::Read { .. } | Work::Write { .. } | Work::Spans { .. } => {
                self.advance(io, x, i, res)
            }
            Work::Open { .. } => {
                let f = &mut self.files[r.file as usize];
                if res < 0 {
                    (f.state, f.generation) = (FState::Free, f.generation.wrapping_add(1));
                    f.poisoned = false;
                } else {
                    f.state = FState::Open;
                }
                plain()
            }
            Work::Close => {
                let f = &mut self.files[r.file as usize];
                (f.state, f.generation) = (FState::Free, f.generation.wrapping_add(1));
                f.poisoned = false;
                plain()
            }
            Work::Sync { .. } if res < 0 => {
                let e = sync_error(errno_of(res));
                if matches!(e, FileError::SyncFailed { .. }) {
                    self.poison(io, x, r.file);
                }
                Outcome::Done(Err(e))
            }
            Work::Path {
                kind: PathKind::Status,
                ..
            } if res >= 0 => {
                let at = STATUS_AREA as usize;
                Outcome::Done(Ok(file_size(&io.buf(self.small(i))[..at])))
            }
            Work::Sync { .. } | Work::Reserve { .. } | Work::Punch { .. } | Work::Path { .. } => {
                plain()
            }
        }
    }

    /// A transfer's completion: whole, more to move, or its end (contract
    /// C6).
    fn advance<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, i: u32, res: i32) -> Outcome {
        let r = self.recs[i as usize];
        let flags = self.files[r.file as usize].flags;
        let write = r.work.is_write();
        if res < 0 {
            let durable = write && flags.dsync();
            if durable {
                self.poison(io, x, r.file);
            }
            return Outcome::Done(Err(transfer_error(r.done, errno_of(res), durable)));
        }
        let len = r.work.len();
        let n = u64::from(res.unsigned_abs());
        debug_assert!(r.done + n <= len, "a transfer moved more than it asked");
        let total = (r.done + n).min(len);
        if total == len {
            return Outcome::Done(Ok(len));
        }
        let bs = u64::from(self.limits.block_size()).max(1);
        if write {
            // Direct I/O resumes from a block boundary: the bytes past it
            // are written again from the same buffer.
            let next = if flags.direct() {
                total - total % bs
            } else {
                total
            };
            if n == 0 || next <= r.done {
                return Outcome::Done(Err(ended(r.done, ErrorKind::NoProgress)));
            }
            self.recs[i as usize].done = next;
            return Outcome::More;
        }
        if n == 0 || (flags.direct() && !total.is_multiple_of(bs)) {
            // The end of the file.
            return Outcome::Done(Ok(total));
        }
        self.recs[i as usize].done = total;
        Outcome::More
    }

    /// Reports record `i` with `result`: its holds are released and its
    /// event queued; a close waiting for it goes.
    pub(crate) fn finish<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        i: u32,
        result: Result<u64, FileError>,
    ) {
        self.release_holds(x, i);
        let r = &mut self.recs[i as usize];
        (r.state, r.result) = (OState::Reported, result);
        (r.lead, r.next, r.out) = (NONE, NONE, 0);
        let f = r.file;
        self.events.push_back(i);
        if i >= self.ops() || f == NONE {
            return;
        }
        let close = self.ops() + f;
        let file = &mut self.files[f as usize];
        file.active -= 1;
        if file.state == FState::Closing
            && file.active == 0
            && self.recs[close as usize].state == OState::Held
        {
            self.launch(io, close);
        }
    }

    /// Releases the holds record `i` has on extents.
    pub(crate) fn release_holds(&mut self, x: &mut Extents, i: u32) {
        match self.recs[i as usize].work {
            Work::Read { extent, .. } | Work::Write { extent, .. } => self.release(x, extent),
            Work::Spans { count, .. } => {
                let row = i as usize * self.config.sizes.spans_max as usize;
                self.release_spans(x, row, count as usize);
            }
            Work::Open { .. }
            | Work::Close
            | Work::Sync { .. }
            | Work::Reserve { .. }
            | Work::Punch { .. }
            | Work::Path { .. } => {}
        }
    }

    /// File slot `f` is Poisoned: its operations waiting for room that it
    /// may no longer run end now (contract C10).
    fn poison<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, f: u32) {
        if f == NONE || self.files[f as usize].poisoned {
            return;
        }
        self.files[f as usize].poisoned = true;
        let ops = self.ops();
        let ends = |s: &Files, i: u32| {
            let r = s.recs[i as usize];
            r.state == OState::Waiting && r.lead == NONE && r.file == f && s.blocked(i)
        };
        let mut any = false;
        for i in 0..ops {
            if ends(self, i) {
                // Marked, so that they leave the queue before they report.
                self.recs[i as usize].state = OState::Resuming;
                any = true;
            }
        }
        if !any {
            return;
        }
        let recs = &self.recs;
        self.waiting
            .retain(|i| recs[i as usize].state == OState::Waiting);
        for i in 0..ops {
            let r = self.recs[i as usize];
            if r.state == OState::Resuming && r.lead == NONE && r.file == f {
                let e = self.blocked_error(i);
                self.finish(io, x, i, Err(e));
            }
        }
    }
}
