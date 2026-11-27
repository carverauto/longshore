// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The file driven through `longshore::file`: the caller's loop routes
//! completions to the set, polls it and takes its events.

use crate::die::{die, OrDie};
use crate::opts::{Api, Opts};
use crate::pass::Pass;
use longshore::file::{Event, File, FileConfig, FileError, Files, Ticket};
use longshore::io::extents::{Extent, Extents};
use longshore::io::{Completions, Io, Limits, OpenFlags};

/// What an accepted operation of a pass is.
#[derive(Clone, Copy)]
enum Job {
    /// A write or a read from extent `i`, begun at `started`.
    Transfer { i: u32, started: u64 },
    /// A data sync, begun at `started`.
    Sync { started: u64 },
}

/// The set, the extents it is lent, and what each ticket in flight is.
pub(crate) struct Set {
    files: Files,
    exts: Box<[Extent]>,
    free: Vec<u32>,
    jobs: Box<[Option<(Ticket, Job)>]>,
}

impl Set {
    /// The set of `config`, planned into `limits`, writing from and reading
    /// into `exts`, which the caller holds.
    pub(crate) fn new(config: FileConfig, limits: &Limits, exts: &[Extent]) -> Set {
        // A job a record, keyed by ticket index: one a token.
        let ops = usize::try_from(config.tokens()).or_die("records");
        Set {
            files: Files::new(config, limits),
            exts: exts.into(),
            free: Vec::with_capacity(exts.len()),
            jobs: vec![None; ops].into_boxed_slice(),
        }
    }

    /// A measured run of every pass `o` asks for, each reported.
    pub(crate) fn run<IO: Io>(
        &mut self,
        io: &mut IO,
        (out, x): (&mut Completions, &mut Extents),
        o: &Opts,
        run: u32,
    ) {
        let flags = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_direct(o.direct)
            .with_dsync(o.dsync);
        let opening = self.files.open(io, &o.path, flags).or_die("open");
        let (file, r) = self.wait(io, out, x, opening);
        let _: u64 = r.or_die("open");
        let file = file.or_die("the open's file");
        for &write in o.passes() {
            let mut p = Pass::new(o, write, false, crate::seed(run, write));
            self.pass(io, (out, x), file, &mut p);
            p.report(o, Api::File, run);
        }
        let closing = self.files.close(io, file).or_die("close");
        let _: u64 = self.wait(io, out, x, closing).1.or_die("close");
    }

    /// One pass, its syncs included.
    fn pass<IO: Io>(
        &mut self,
        io: &mut IO,
        (out, x): (&mut Completions, &mut Extents),
        file: File,
        p: &mut Pass,
    ) {
        self.free.clear();
        self.free.extend((0..self.exts.len() as u32).rev());
        let mut syncing = false;
        loop {
            self.issue(io, x, file, p);
            if self.free.len() == self.exts.len() && !syncing {
                break;
            }
            io.tick(true, out).or_die("tick");
            self.route(io, out, x);
            while let Some(ev) = self.files.next_event() {
                self.event(ev, p, &mut syncing);
            }
            if !syncing && p.sync_due() {
                let started = p.now();
                let t = self.files.sync(io, file, true).or_die("sync");
                self.track(t, Job::Sync { started });
                syncing = true;
            }
        }
        if p.final_sync() {
            let started = p.now();
            let t = self.files.sync(io, file, true).or_die("sync");
            let _: u64 = self.wait(io, out, x, t).1.or_die("sync");
            p.sync_done(started);
        }
    }

    /// Ask for an operation into or from every free extent while the pass
    /// has some.
    fn issue<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, file: File, p: &mut Pass) {
        while let Some(&i) = self.free.last() {
            let Some(file_off) = p.next() else { break };
            let _: Option<u32> = self.free.pop();
            let range = (self.exts[i as usize], 0, p.len());
            let started = p.now();
            let t = if p.write {
                self.files.write_at(io, x, file, range, file_off)
            } else {
                self.files.read_at(io, x, file, range, file_off)
            };
            self.track(
                t.or_die("write_at or read_at"),
                Job::Transfer { i, started },
            );
        }
    }

    fn track(&mut self, t: Ticket, job: Job) {
        let slot = self
            .jobs
            .get_mut(t.index() as usize)
            .or_die("a ticket inside the plan");
        if slot.is_some() {
            die("a ticket of an operation in flight");
        }
        *slot = Some((t, job));
    }

    /// Hand every completion to the set, and let it submit what waited.
    fn route<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, x: &mut Extents) {
        while let Some(c) = out.pop() {
            if !self.files.on_completion(io, x, c) {
                die("a completion that is not the set's");
            }
        }
        self.files.poll(io, x);
    }

    /// Take the event `ev` of a pass.
    fn event(&mut self, ev: Event, p: &mut Pass, syncing: &mut bool) {
        let Event::Done { ticket, result, .. } = ev;
        let job = self
            .jobs
            .get_mut(ticket.index() as usize)
            .and_then(Option::take)
            .filter(|&(t, _)| t == ticket)
            .or_die("an event of a ticket in flight")
            .1;
        let n = result.or_die("a write, read or sync");
        match job {
            Job::Transfer { i, started } => {
                if n != u64::from(p.len()) {
                    die(format_args!("a transfer of {n} bytes of {}", p.len()));
                }
                p.op_done(started);
                self.free.push(i);
            }
            Job::Sync { started } => {
                p.sync_done(started);
                *syncing = false;
            }
        }
    }

    /// Run the loop until the event of `t` alone, which no other operation
    /// is in flight beside.
    fn wait<IO: Io>(
        &mut self,
        io: &mut IO,
        out: &mut Completions,
        x: &mut Extents,
        t: Ticket,
    ) -> (Option<File>, Result<u64, FileError>) {
        loop {
            if let Some(Event::Done {
                ticket,
                file,
                result,
            }) = self.files.next_event()
            {
                if ticket != t {
                    die("an event of another ticket");
                }
                return (file, result);
            }
            io.tick(true, out).or_die("tick");
            self.route(io, out, x);
        }
    }
}
