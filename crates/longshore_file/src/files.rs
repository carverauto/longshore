// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The set: its calls, driven from the caller's loop (contract C1 to C12).
//! Skeleton: every call refuses until group 2 fills it.

use crate::{Event, File, FileConfig, FileError, FileUsage, Span, Step, Ticket};
use longshore_common::io::extents::{Extent, Extents};
use longshore_common::io::{Completion, Io, Limits, OpenFlags};

/// A part of an extent: the extent, where in it, and how many bytes.
pub type Range = (Extent, u32, u32);

/// A fixed set of files and operations over an `Io` backend.
pub struct Files {
    config: FileConfig,
}

impl Files {
    /// The set for `config`, planned into `limits`; every table is made
    /// here, and nothing is allocated after.
    pub fn new(config: FileConfig, _limits: &Limits) -> Files {
        Files { config }
    }

    /// The plan this set was built from.
    pub const fn config(&self) -> &FileConfig {
        &self.config
    }

    /// Open `path`; the event carries the new [`File`].
    pub fn open<IO: Io>(
        &mut self,
        _io: &mut IO,
        _path: &str,
        _flags: OpenFlags,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Close `file` once its other operations have reported; never `Full`.
    pub fn close<IO: Io>(&mut self, _io: &mut IO, _file: File) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Read `range` from `file` at `file_off`, whole but at end of file.
    pub fn read_at<IO: Io>(
        &mut self,
        _io: &mut IO,
        _x: &mut Extents,
        _file: File,
        _range: Range,
        _file_off: u64,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Write `range` to `file` at `file_off`, whole or with an error.
    pub fn write_at<IO: Io>(
        &mut self,
        _io: &mut IO,
        _x: &mut Extents,
        _file: File,
        _range: Range,
        _file_off: u64,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Write `spans` in order to `file` at `file_off` as one write.
    pub fn write_spans<IO: Io>(
        &mut self,
        _io: &mut IO,
        _x: &mut Extents,
        _file: File,
        _spans: &[Span],
        _file_off: u64,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Sync `file`'s data, and its metadata unless `data_only`.
    pub fn sync<IO: Io>(
        &mut self,
        _io: &mut IO,
        _file: File,
        _data_only: bool,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Reserve `len` bytes of `file` from `off`.
    pub fn reserve<IO: Io>(
        &mut self,
        _io: &mut IO,
        _file: File,
        (_off, _len): (u64, u64),
        _keep_size: bool,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Deallocate `len` bytes of `file` from `off`, whole blocks.
    pub fn punch<IO: Io>(
        &mut self,
        _io: &mut IO,
        _file: File,
        (_off, _len): (u64, u64),
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// The size of the file at `path`.
    pub fn status<IO: Io>(&mut self, _io: &mut IO, _path: &str) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Rename `from` to `to`.
    pub fn rename<IO: Io>(
        &mut self,
        _io: &mut IO,
        _from: &str,
        _to: &str,
    ) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Remove the file at `path`.
    pub fn unlink<IO: Io>(&mut self, _io: &mut IO, _path: &str) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Make a directory at `path`.
    pub fn mkdir<IO: Io>(&mut self, _io: &mut IO, _path: &str) -> Result<Ticket, FileError> {
        Err(FileError::Full)
    }

    /// Submit `steps` as one ordered sequence; their tickets go into
    /// `tickets`, in order, and their count is returned.
    pub fn sequence<IO: Io>(
        &mut self,
        _io: &mut IO,
        _x: &mut Extents,
        _steps: &[Step<'_>],
        _tickets: &mut [Ticket],
    ) -> Result<usize, FileError> {
        Err(FileError::Full)
    }

    /// Take `c` if it is the set's; whether it was.
    pub fn on_completion<IO: Io>(
        &mut self,
        _io: &mut IO,
        _x: &mut Extents,
        _c: Completion,
    ) -> bool {
        false
    }

    /// Submit what waited for room.
    pub fn poll<IO: Io>(&mut self, _io: &mut IO, _x: &mut Extents) {}

    /// The next outcome, if any.
    pub fn next_event(&mut self) -> Option<Event> {
        None
    }

    /// The backend lost power: free every slot, release every hold, report
    /// nothing for what was in flight; allocates nothing.
    pub fn power_lost(&mut self, _x: &mut Extents) {}

    /// What the set holds now.
    pub fn usage(&self) -> FileUsage {
        FileUsage::default()
    }
}
