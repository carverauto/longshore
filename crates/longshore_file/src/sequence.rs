// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ordered sequences on the backend's chains (contract C7).
//!
//! A sequence goes to the backend as one chain. Its steps' completions are
//! kept until every step of the chain has completed; then they are read in
//! order. Whole steps report; the first that failed reports and the rest
//! are cancelled; the first that has more to move, or that the backend did
//! not run because the step before it was short, leads a new chain of
//! itself and the rest.

use crate::complete::Outcome;
use crate::errno::{errno_of, ECANCELED};
use crate::state::{OState, PathKind, Work, NONE};
use crate::{FileError, Files, Step, Ticket};
use longshore_common::io::extents::Extents;
use longshore_common::io::Io;

impl Files {
    /// Accepts `steps` as one sequence, or refuses it whole.
    pub(crate) fn accept_sequence<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        steps: &[Step<'_>],
        tickets: &mut [Ticket],
    ) -> Result<usize, FileError> {
        let n = steps.len();
        if n == 0 {
            return Ok(0);
        }
        if n > self.config.sizes.sequence_max as usize || tickets.len() < n {
            return Err(FileError::TooLarge);
        }
        for (k, step) in steps.iter().enumerate() {
            match self.accept_step(io, x, step) {
                Ok(i) => tickets[k] = self.ticket(i),
                Err(e) => {
                    for t in &tickets[..k] {
                        self.release_holds(x, t.index);
                        self.untake(t.index);
                    }
                    return Err(e);
                }
            }
        }
        let lead = tickets[0].index;
        for k in 0..n {
            let r = &mut self.recs[tickets[k].index as usize];
            r.lead = lead;
            r.next = if k + 1 < n {
                tickets[k + 1].index
            } else {
                NONE
            };
        }
        self.launch_chain(io, x, lead);
        Ok(n)
    }

    /// One step, accepted as its own call would be.
    fn accept_step<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        step: &Step<'_>,
    ) -> Result<u32, FileError> {
        match *step {
            Step::Read {
                file,
                extent,
                at,
                len,
                file_off,
            } => self.accept_rw(x, file, (extent, at, len), file_off, false),
            Step::Write {
                file,
                extent,
                at,
                len,
                file_off,
            } => self.accept_rw(x, file, (extent, at, len), file_off, true),
            Step::WriteSpans {
                file,
                spans,
                file_off,
            } => self.accept_spans(x, file, spans, file_off),
            Step::Sync { file, data_only } => self.accept_file_op(file, Work::Sync { data_only }),
            Step::Reserve {
                file,
                off,
                len,
                keep_size,
            } => {
                let work = Work::Reserve {
                    off,
                    len,
                    keep_size,
                };
                self.accept_file_op(file, work)
            }
            Step::Punch { file, off, len } => self.accept_file_op(file, Work::Punch { off, len }),
            Step::Status { path } => self.accept_path_op(io, PathKind::Status, path, None),
            Step::Rename { from, to } => self.accept_path_op(io, PathKind::Rename, from, Some(to)),
            Step::Unlink { path } => self.accept_path_op(io, PathKind::Unlink, path, None),
            Step::Mkdir { path } => self.accept_path_op(io, PathKind::Mkdir, path, None),
        }
    }

    /// Completion `res` of step `i`; the chain is read once all of it has
    /// completed.
    pub(crate) fn step_completed<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        i: u32,
        res: i32,
    ) {
        let lead = self.recs[i as usize].lead;
        let r = &mut self.recs[i as usize];
        (r.raw, r.state) = (res, OState::Resuming);
        let l = &mut self.recs[lead as usize];
        l.out = l.out.saturating_sub(1);
        if l.out == 0 {
            self.read_chain(io, x, lead);
        }
    }

    /// Every step of the chain led by `lead` has completed: reads them in
    /// order.
    fn read_chain<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, lead: u32) {
        if self.recs[lead as usize].raw == -ECANCELED {
            self.refused(io, x, lead);
            return;
        }
        let mut r = lead;
        while r != NONE {
            let next = self.recs[r as usize].next;
            let res = self.recs[r as usize].raw;
            let outcome = if res == -ECANCELED {
                // Not run: the step before it was short, or at its end.
                Outcome::More
            } else {
                self.outcome(io, x, r, res)
            };
            match outcome {
                Outcome::Done(Ok(v)) => self.finish(io, x, r, Ok(v)),
                Outcome::Done(Err(e)) => {
                    self.finish(io, x, r, Err(e));
                    self.cancel_from(io, x, next);
                    return;
                }
                Outcome::More => {
                    let mut k = r;
                    while k != NONE {
                        self.recs[k as usize].lead = r;
                        k = self.recs[k as usize].next;
                    }
                    self.launch_chain(io, x, r);
                    return;
                }
            }
            r = next;
        }
    }

    /// The backend refused the chain as a whole, which the set's checks
    /// are there to prevent: the step it named reports `Invalid`, the rest
    /// `Cancelled`.
    fn refused<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, lead: u32) {
        let mut r = lead;
        while r != NONE {
            let next = self.recs[r as usize].next;
            let res = self.recs[r as usize].raw;
            let e = if res < 0 && res != -ECANCELED {
                FileError::Invalid {
                    errno: errno_of(res),
                }
            } else {
                FileError::Cancelled
            };
            self.finish(io, x, r, Err(e));
            r = next;
        }
    }
}
