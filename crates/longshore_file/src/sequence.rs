// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ordered sequences on the backend's chains (contract C7).
//!
//! A sequence goes to the backend as chains: one, unless it has path steps
//! (status, rename, unlink, mkdir), each of which ends its chain, since a
//! kernel need not end a link when one of them fails. A chain's
//! completions are kept until every step of it has completed; then they
//! are read in order. Whole steps report; the first that failed reports
//! and the rest are cancelled (a step the backend ran all the same reports
//! what it did); the first that has more to move, or that the backend did
//! not run because the step before it was short, leads a new chain of
//! itself and the rest, as does the step after a chain whose steps were
//! all whole.

use crate::complete::Outcome;
use crate::errno::{ended, errno_of, transfer_error, ECANCELED};
use crate::state::{OState, PathKind, Work, NONE};
use crate::{ErrorKind, FileError, Files, Step, Ticket};
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
                    for t in &mut tickets[..k] {
                        self.release_holds(x, t.index);
                        self.untake(t.index);
                        *t = Ticket::NONE;
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
    ///
    /// A chain the backend refused at submission is told apart by its
    /// lead: a backend runs the first step of a chain it took, so the lead
    /// comes back `-ECANCELED` only when the backend refused a later step
    /// and cancelled the rest with it. A lead the backend refused itself
    /// reports its own errno as any failed step does.
    fn read_chain<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, lead: u32) {
        let sent = self.recs[lead as usize].sent;
        if self.recs[lead as usize].raw == -ECANCELED {
            self.refused(io, x, lead, sent);
            return;
        }
        let mut r = lead;
        for k in 0..sent {
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
                    self.end_after(io, x, next, sent - k - 1);
                    return;
                }
                Outcome::More => {
                    self.relaunch(io, x, r);
                    return;
                }
            }
            r = next;
        }
        // Every step of the chain whole: the rest of the sequence goes.
        if r != NONE {
            self.relaunch(io, x, r);
        }
    }

    /// The steps from `r` on, a chain of their own led by `r`.
    fn relaunch<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, r: u32) {
        let mut k = r;
        while k != NONE {
            self.recs[k as usize].lead = r;
            k = self.recs[k as usize].next;
        }
        self.launch_chain(io, x, r);
    }

    /// Ends the sequence after a failed step: the `ran` steps from `r` on
    /// were in its chain, and report `Cancelled` if the backend did not
    /// run them, or what they did if it did; the steps after them are
    /// cancelled.
    fn end_after<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, mut r: u32, ran: u32) {
        for _ in 0..ran {
            let next = self.recs[r as usize].next;
            let res = self.recs[r as usize].raw;
            let result = if res == -ECANCELED {
                Err(FileError::Cancelled)
            } else {
                match self.outcome(io, x, r, res) {
                    Outcome::Done(result) => result,
                    // A transfer that ran short, with no sequence left to
                    // finish it in.
                    Outcome::More => Err(ended(
                        self.recs[r as usize].reach,
                        ErrorKind::Os { errno: ECANCELED },
                    )),
                }
            };
            self.finish(io, x, r, result);
            r = next;
        }
        self.cancel_from(io, x, r);
    }

    /// The backend refused the chain as a whole: the step it named
    /// reports its errno as a plain error (`Os` for a resource the backend
    /// had none of, such as a gather row; `Invalid` for what the set's
    /// checks are there to prevent), the rest `Cancelled`. A step resumed
    /// in this chain keeps the bytes it moved before (contract C6): it
    /// ends `Partial`. Nothing ran, so no file is poisoned.
    fn refused<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, lead: u32, sent: u32) {
        let mut r = lead;
        for _ in 0..sent {
            let next = self.recs[r as usize].next;
            let (res, reach) = (self.recs[r as usize].raw, self.recs[r as usize].reach);
            let e = if res < 0 && res != -ECANCELED {
                transfer_error(reach, errno_of(res), false)
            } else if reach > 0 {
                ended(reach, ErrorKind::Os { errno: ECANCELED })
            } else {
                FileError::Cancelled
            };
            self.finish(io, x, r, Err(e));
            r = next;
        }
        self.cancel_from(io, x, r);
    }
}
