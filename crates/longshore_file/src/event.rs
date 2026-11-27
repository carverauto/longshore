// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the set reports (contract C4).

use crate::{File, FileError, Ticket};

/// An operation's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// The operation of `ticket` is over.
    Done {
        /// The operation.
        ticket: Ticket,
        /// The new handle for an open, the file for a file operation,
        /// `None` for a path operation.
        file: Option<File>,
        /// Bytes transferred, the size for a status, 0 otherwise; or why it
        /// failed.
        result: Result<u64, FileError>,
    },
}

/// What the set holds now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FileUsage {
    pub(crate) files: u32,
    pub(crate) ops: u32,
    pub(crate) waiting: u32,
    pub(crate) broken_holds: u32,
}

impl FileUsage {
    /// File slots not Free.
    pub const fn files(&self) -> u32 {
        self.files
    }

    /// Operation slots not Free.
    pub const fn ops(&self) -> u32 {
        self.ops
    }

    /// Operations waiting for room at the backend.
    pub const fn waiting(&self) -> u32 {
        self.waiting
    }

    /// Releases of the set's own holds that failed: always 0 unless the
    /// caller released a hold that was the set's (contract C8).
    pub const fn broken_holds(&self) -> u32 {
        self.broken_holds
    }
}
