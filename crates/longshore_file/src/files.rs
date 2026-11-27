// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The set: its calls, driven from the caller's loop (contract C1 to C12).
//!
//! Records `0..ops` are the operation slots: one reserved for each file
//! slot, then the path slots, then the shared ones. Records `ops..ops +
//! files` are each file slot's own, for its open and its close, whose room
//! at the backend is the reserved slot's: a file opens and closes with
//! nothing else of it in flight.

use crate::state::{FileRec, OpRec, PathKind, Ring, Stack, Work};
use crate::{Event, File, FileConfig, FileError, FileUsage, Span, Step, Ticket};
use longshore_common::io::extents::{Extent, Extents};
use longshore_common::io::{BufId, Completion, Io, Limits, Op, OpenFlags, Token};

/// A part of an extent: the extent, where in it, and how many bytes.
pub type Range = (Extent, u32, u32);

/// A fixed set of files and operations over an `Io` backend.
pub struct Files {
    pub(crate) config: FileConfig,
    pub(crate) limits: Limits,
    pub(crate) files: Box<[FileRec]>,
    pub(crate) recs: Box<[OpRec]>,
    /// `spans_max` spans for every operation slot: a gathered write's.
    pub(crate) spans: Box<[Span]>,
    /// Room to build one sequence's chain.
    pub(crate) chain: Box<[(Token, Op)]>,
    /// Work waiting for room at the backend, first in first out; a
    /// sequence by the step that leads it.
    pub(crate) waiting: Ring,
    /// Records with an event to take, in the order they were reported.
    pub(crate) events: Ring,
    pub(crate) shared_free: Stack,
    pub(crate) path_free: Stack,
    /// File slots Free whose own record is Free too.
    pub(crate) open_free: Stack,
    /// The first of the set's small buffers.
    pub(crate) small_first: u32,
    pub(crate) broken_holds: u32,
}

impl Files {
    /// The set for `config`, planned into `limits`; every table is made
    /// here, and nothing is allocated after.
    pub fn new(config: FileConfig, limits: &Limits) -> Files {
        let s = config.sizes;
        let ops = s.files + s.paths + s.shared;
        let records = (ops + s.files) as usize;
        let none = Span::Page {
            page: BufId::new(0),
            off: 0,
            len: 0,
        };
        let small_first = limits.small(config.small_base).get();
        debug_assert!(
            config.small_base + s.files + s.paths + ops <= limits.small_buffers(),
            "the limits were not planned for this set"
        );
        Files {
            config,
            limits: *limits,
            files: vec![FileRec::FREE; s.files as usize].into_boxed_slice(),
            recs: vec![OpRec::FREE; records].into_boxed_slice(),
            spans: vec![none; ops as usize * s.spans_max as usize].into_boxed_slice(),
            chain: vec![(Token::new(0), Op::AwaitWake); s.sequence_max as usize].into_boxed_slice(),
            waiting: Ring::new(records),
            events: Ring::new(records),
            shared_free: Stack::full(s.files + s.paths, ops),
            path_free: Stack::full(s.files, s.files + s.paths),
            open_free: Stack::full(0, s.files),
            small_first,
            broken_holds: 0,
        }
    }

    /// The plan this set was built from.
    pub const fn config(&self) -> &FileConfig {
        &self.config
    }

    /// Open `path`; the event carries the new [`File`].
    pub fn open<IO: Io>(
        &mut self,
        io: &mut IO,
        path: &str,
        flags: OpenFlags,
    ) -> Result<Ticket, FileError> {
        let len = self.path_len(path)?;
        let f = self.open_free.pop().ok_or(FileError::Full)?;
        self.put_path(io, self.file_path_buf(f), 0, path);
        let i = self.begin_open(f, Work::Open { flags, len }, flags);
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Close `file` once its other operations have reported; never `Full`.
    pub fn close<IO: Io>(&mut self, io: &mut IO, file: File) -> Result<Ticket, FileError> {
        let f = self.live(file)?;
        let i = self.begin_close(f)?;
        if self.files[f as usize].active == 0 {
            self.launch(io, i);
        }
        Ok(self.ticket(i))
    }

    /// Read `range` from `file` at `file_off`, whole but at end of file.
    pub fn read_at<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        file: File,
        range: Range,
        file_off: u64,
    ) -> Result<Ticket, FileError> {
        let i = self.accept_rw(x, file, range, file_off, false)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Write `range` to `file` at `file_off`, whole or with an error.
    pub fn write_at<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        file: File,
        range: Range,
        file_off: u64,
    ) -> Result<Ticket, FileError> {
        let i = self.accept_rw(x, file, range, file_off, true)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Write `spans` in order to `file` at `file_off` as one write.
    pub fn write_spans<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        file: File,
        spans: &[Span],
        file_off: u64,
    ) -> Result<Ticket, FileError> {
        let i = self.accept_spans(x, file, spans, file_off)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Sync `file`'s data, and its metadata unless `data_only`.
    pub fn sync<IO: Io>(
        &mut self,
        io: &mut IO,
        file: File,
        data_only: bool,
    ) -> Result<Ticket, FileError> {
        let i = self.accept_file_op(file, Work::Sync { data_only })?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Reserve `len` bytes of `file` from `off`.
    pub fn reserve<IO: Io>(
        &mut self,
        io: &mut IO,
        file: File,
        (off, len): (u64, u64),
        keep_size: bool,
    ) -> Result<Ticket, FileError> {
        let work = Work::Reserve {
            off,
            len,
            keep_size,
        };
        let i = self.accept_file_op(file, work)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Deallocate `len` bytes of `file` from `off`, whole blocks.
    pub fn punch<IO: Io>(
        &mut self,
        io: &mut IO,
        file: File,
        (off, len): (u64, u64),
    ) -> Result<Ticket, FileError> {
        let i = self.accept_file_op(file, Work::Punch { off, len })?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// The size of the file at `path`.
    pub fn status<IO: Io>(&mut self, io: &mut IO, path: &str) -> Result<Ticket, FileError> {
        let i = self.accept_path_op(io, PathKind::Status, path, None)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Rename `from` to `to`.
    pub fn rename<IO: Io>(
        &mut self,
        io: &mut IO,
        from: &str,
        to: &str,
    ) -> Result<Ticket, FileError> {
        let i = self.accept_path_op(io, PathKind::Rename, from, Some(to))?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Remove the file at `path`.
    pub fn unlink<IO: Io>(&mut self, io: &mut IO, path: &str) -> Result<Ticket, FileError> {
        let i = self.accept_path_op(io, PathKind::Unlink, path, None)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Make a directory at `path`.
    pub fn mkdir<IO: Io>(&mut self, io: &mut IO, path: &str) -> Result<Ticket, FileError> {
        let i = self.accept_path_op(io, PathKind::Mkdir, path, None)?;
        self.launch(io, i);
        Ok(self.ticket(i))
    }

    /// Submit `steps` as one ordered sequence; their tickets go into
    /// `tickets`, in order, and their count is returned.
    pub fn sequence<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        steps: &[Step<'_>],
        tickets: &mut [Ticket],
    ) -> Result<usize, FileError> {
        self.accept_sequence(io, x, steps, tickets)
    }

    /// Take `c` if it is the set's; whether it was.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, c: Completion) -> bool {
        let Some(i) = self.record_of(c.token()) else {
            return false;
        };
        self.completed(io, x, i, c.result());
        // The completion gave back room at the backend: what waited for
        // room goes now, so that no accepted work waits while the backend
        // could take it.
        self.submit_waiting(io, x);
        true
    }

    /// Submit what waited for room.
    pub fn poll<IO: Io>(&mut self, io: &mut IO, x: &mut Extents) {
        self.submit_waiting(io, x);
    }

    /// The next outcome, if any.
    pub fn next_event(&mut self) -> Option<Event> {
        self.take_event()
    }

    /// The backend lost power: free every slot, release every hold, report
    /// nothing for what was in flight; allocates nothing.
    pub fn power_lost(&mut self, x: &mut Extents) {
        self.reset(x);
    }

    /// What the set holds now.
    pub fn usage(&self) -> FileUsage {
        self.count_usage()
    }

    /// The ticket of record `i` as it is now.
    pub(crate) fn ticket(&self, i: u32) -> Ticket {
        Ticket {
            index: i,
            generation: self.recs[i as usize].generation,
        }
    }
}
