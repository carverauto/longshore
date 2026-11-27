// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A set on a spied backend, with the caller's loop around it: turn the
//! backend, route its completions to the set, poll, and keep the events
//! until a check asks for them. Turning allocates nothing.

use super::spy::{Spy, STAGED};
use super::{fail, must, some};
use crate::{Event, File, FileConfig, FileError, FileSizes, FileUsage, Files, Ticket};
use longshore_common::io::extents::{Extent, Extents};
use longshore_common::io::{Completion, Completions, Io, Limits, OpenFlags};

/// Turns a check waits for an event before it fails.
const TURNS: u32 = 20_000;
/// Events kept at most (the set never has more than its operation slots).
const EVENTS: usize = 1024;

/// One set on one spied backend, and the caller's extents.
pub struct Rig<IO: Io> {
    io: Spy<IO>,
    files: Files,
    x: Extents,
    limits: Limits,
    config: FileConfig,
    out: Completions,
    events: Vec<(u64, Event)>,
    seq: u64,
    foreign: Vec<Completion>,
}

impl<IO: Io> Rig<IO> {
    /// `sizes` planned into `base`, the backend `make` builds from the
    /// planned limits, and extents over its block-aligned class.
    pub fn new(make: &mut impl FnMut(Limits) -> IO, base: Limits, sizes: FileSizes) -> Rig<IO> {
        let (limits, config) = must(FileConfig::plan(base, sizes), "the plan");
        let io = Spy::new(make(limits), 4096);
        Rig::over(io, limits, config)
    }

    /// The same over a backend already built from `limits`, which `config`
    /// was planned into.
    pub fn over(io: Spy<IO>, limits: Limits, config: FileConfig) -> Rig<IO> {
        let files = Files::new(config, &limits);
        let x = Extents::new(limits.aligned(0), limits.aligned_buffers());
        Rig {
            io,
            files,
            x,
            limits,
            config,
            out: Completions::new(STAGED),
            events: Vec::with_capacity(EVENTS),
            seq: 0,
            foreign: Vec::with_capacity(STAGED),
        }
    }

    /// The spied backend.
    pub fn io(&mut self) -> &mut Spy<IO> {
        &mut self.io
    }

    /// The set.
    pub fn files(&mut self) -> &mut Files {
        &mut self.files
    }

    /// The caller's extents.
    pub fn x(&mut self) -> &mut Extents {
        &mut self.x
    }

    /// The backend, the set and the extents at once.
    pub fn parts(&mut self) -> (&mut Spy<IO>, &mut Files, &mut Extents) {
        (&mut self.io, &mut self.files, &mut self.x)
    }

    /// The planned limits the backend was built from.
    pub const fn limits(&self) -> Limits {
        self.limits
    }

    /// The set's plan.
    pub const fn config(&self) -> FileConfig {
        self.config
    }

    /// What the set holds now.
    pub fn usage(&self) -> FileUsage {
        self.files.usage()
    }

    /// Call the set; it must not turn the backend itself (C1).
    pub fn call<V>(&mut self, f: impl FnOnce(&mut Files, &mut Spy<IO>, &mut Extents) -> V) -> V {
        let ticks = self.io.ticks();
        let v = f(&mut self.files, &mut self.io, &mut self.x);
        if self.io.ticks() != ticks {
            fail(format_args!("the set turned the backend itself"));
        }
        v
    }

    /// Turn the backend once, waiting only when something is in flight,
    /// route what completed, poll, and keep the events.
    pub fn turn(&mut self) {
        let wait = self.io.outstanding() > 0;
        must(self.io.tick(wait, &mut self.out), "tick");
        while let Some(c) = self.out.pop() {
            let ticks = self.io.ticks();
            if !self.files.on_completion(&mut self.io, &mut self.x, c) {
                self.foreign.push(c);
            }
            if self.io.ticks() != ticks {
                fail(format_args!("the set turned the backend in on_completion"));
            }
        }
        self.call(|f, io, x| f.poll(io, x));
        self.collect();
    }

    /// Keep every event the set has.
    pub fn collect(&mut self) {
        while let Some(e) = self.files.next_event() {
            self.seq += 1;
            self.events.push((self.seq, e));
        }
    }

    /// Completions the set did not take, oldest first; taken from the rig.
    pub fn take_foreign(&mut self) -> Option<Completion> {
        (!self.foreign.is_empty()).then(|| self.foreign.remove(0))
    }

    /// The event of `t` and its place in the order of all events, once it
    /// has come.
    pub fn wait(&mut self, t: Ticket) -> (u64, Event) {
        for _ in 0..TURNS {
            self.collect();
            let at = self.events.iter().position(|(_, e)| ticket(*e) == t);
            if let Some(at) = at {
                return self.events.remove(at);
            }
            self.turn();
        }
        fail(format_args!(
            "{t:?} never reported; usage {:?}",
            self.usage()
        ))
    }

    /// The result of `t`, once it has come.
    pub fn done(&mut self, t: Ticket) -> Result<u64, FileError> {
        let Event::Done { result, .. } = self.wait(t).1;
        result
    }

    /// Ask for an operation with `call`, which must be accepted, and wait
    /// for its result.
    pub fn run(
        &mut self,
        what: &str,
        call: impl FnOnce(&mut Files, &mut Spy<IO>, &mut Extents) -> Result<Ticket, FileError>,
    ) -> Result<u64, FileError> {
        let t = must(self.call(call), what);
        self.done(t)
    }

    /// Turn until nothing is in flight at the backend or waiting in the set.
    pub fn settle(&mut self) {
        for _ in 0..TURNS {
            self.collect();
            if self.io.outstanding() == 0 && self.usage().waiting() == 0 {
                return;
            }
            self.turn();
        }
        fail(format_args!("never settled: {:?}", self.usage()))
    }

    /// The oldest event kept and its place in the order of all events;
    /// taken from the rig. Allocates nothing.
    pub fn pop_event(&mut self) -> Option<(u64, Event)> {
        (!self.events.is_empty()).then(|| self.events.remove(0))
    }

    /// The events kept so far, oldest first; taken from the rig.
    pub fn take_events(&mut self) -> Vec<(u64, Event)> {
        core::mem::take(&mut self.events)
    }

    /// F10, and C1's clock: settled, with no event left, every slot's state
    /// agrees: `open` files, no operation slot in use, no hold broken, no
    /// completion that was not the set's, and the clock never read.
    pub fn at_rest(&mut self, open: u32) {
        self.settle();
        if !self.events.is_empty() {
            fail(format_args!("events nobody asked for: {:?}", self.events));
        }
        let u = self.usage();
        let want = (open, 0, 0, 0);
        if (u.files(), u.ops(), u.waiting(), u.broken_holds()) != want {
            fail(format_args!("not at rest with {open} files open: {u:?}"));
        }
        if !self.foreign.is_empty() || self.io.clock_reads() != 0 {
            fail(format_args!(
                "{:?} not taken, {} clock reads",
                self.foreign,
                self.io.clock_reads()
            ));
        }
    }

    /// A free extent with the caller's hold on it.
    pub fn extent(&mut self) -> Extent {
        some(self.x.take(), "a free extent")
    }

    /// Release the caller's hold on `e`.
    pub fn release(&mut self, e: Extent) {
        let _freed: bool = must(self.x.release(e), "the caller's hold");
    }

    /// Put `data` into `e` from `at`.
    pub fn fill(&mut self, e: Extent, at: u32, data: &[u8]) {
        let at = at as usize;
        self.io.buf(e.buf())[at..at + data.len()].copy_from_slice(data);
    }

    /// `len` bytes of `e` from `at`.
    pub fn bytes(&mut self, e: Extent, at: u32, len: u32) -> Vec<u8> {
        self.io.buf(e.buf())[at as usize..(at + len) as usize].to_vec()
    }

    /// Open `path`; the open must be accepted.
    pub fn try_open(&mut self, path: &str, flags: OpenFlags) -> Result<File, FileError> {
        let t = must(self.call(|f, io, _| f.open(io, path, flags)), path);
        match self.wait(t).1 {
            Event::Done {
                file: Some(file),
                result: Ok(0),
                ..
            } => Ok(file),
            Event::Done { result: Err(e), .. } => Err(e),
            other @ Event::Done { .. } => fail(format_args!("open {path}: {other:?}")),
        }
    }

    /// Open `path`; it must open.
    pub fn open(&mut self, path: &str, flags: OpenFlags) -> File {
        must(self.try_open(path, flags), path)
    }

    /// Write `data` to `file` at `off` from a fresh extent the caller lets
    /// go of at once: the set's own hold keeps it.
    pub fn write(&mut self, file: File, data: &[u8], off: u64) -> Result<u64, FileError> {
        let e = self.extent();
        self.fill(e, 0, data);
        let len = data.len() as u32;
        let t = self.call(|f, io, x| f.write_at(io, x, file, (e, 0, len), off));
        self.release(e);
        let t = must(t, "write_at");
        let r = self.done(t);
        if self.x.is_live(e) {
            fail(format_args!("the set still holds {e:?} after its event"));
        }
        r
    }

    /// Read `len` bytes of `file` at `off`; the result and the bytes read.
    pub fn read(&mut self, file: File, off: u64, len: u32) -> (Result<u64, FileError>, Vec<u8>) {
        let e = self.extent();
        let t = self.call(|f, io, x| f.read_at(io, x, file, (e, 0, len), off));
        let t = must(t, "read_at");
        let r = self.done(t);
        let n = r.map_or(0, |n| n as u32);
        let got = self.bytes(e, 0, n);
        if self.x.holds(e) != Some(1) {
            fail(format_args!("the set still holds {e:?} after its event"));
        }
        self.release(e);
        (r, got)
    }

    /// Sync `file`.
    pub fn sync(&mut self, file: File, data_only: bool) -> Result<u64, FileError> {
        let t = must(self.call(|f, io, _| f.sync(io, file, data_only)), "sync");
        self.done(t)
    }

    /// Close `file`.
    pub fn close(&mut self, file: File) -> Result<u64, FileError> {
        let t = must(self.call(|f, io, _| f.close(io, file)), "close");
        self.done(t)
    }

    /// The size of the file at `path`.
    pub fn status(&mut self, path: &str) -> Result<u64, FileError> {
        let t = must(self.call(|f, io, _| f.status(io, path)), "status");
        self.done(t)
    }

    /// C11: `call` is refused with `want`, and submits nothing, takes no
    /// slot and no hold, and reports nothing.
    pub fn refused<V: core::fmt::Debug>(
        &mut self,
        what: &str,
        want: FileError,
        call: impl FnOnce(&mut Files, &mut Spy<IO>, &mut Extents) -> Result<V, FileError>,
    ) {
        self.collect();
        let before = (
            self.usage(),
            self.io.submitted(),
            self.x.free_count(),
            self.seq,
        );
        let got = self.call(call);
        if !matches!(&got, Err(e) if *e == want) {
            fail(format_args!("{what}: {got:?}, not {want:?}"));
        }
        self.collect();
        let after = (
            self.usage(),
            self.io.submitted(),
            self.x.free_count(),
            self.seq,
        );
        if after != before {
            fail(format_args!(
                "{what}: a refusal changed {before:?} to {after:?}"
            ));
        }
    }
}

/// The ticket an event names.
pub const fn ticket(e: Event) -> Ticket {
    let Event::Done { ticket, .. } = e;
    ticket
}
