// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a gathered write takes, and the steps of an ordered sequence
//! (contract C5, C7).

use crate::File;
use longshore_common::io::extents::Extent;
use longshore_common::io::BufId;

/// One span of a gathered write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Span {
    /// `len` bytes of an extent from `at`; the set holds the extent while
    /// the write is in flight.
    Extent {
        /// The extent.
        extent: Extent,
        /// Where the bytes start in it.
        at: u32,
        /// How many.
        len: u32,
    },
    /// `len` bytes of a page of the receive area from `off`; the caller
    /// holds the page until the write's event.
    Page {
        /// The page.
        page: BufId,
        /// Where the bytes start in it.
        off: u32,
        /// How many.
        len: u32,
    },
}

/// A step of an ordered sequence: any operation but open and close.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step<'a> {
    /// See [`crate::Files::read_at`].
    Read {
        /// The file.
        file: File,
        /// The extent read into.
        extent: Extent,
        /// Where in the extent.
        at: u32,
        /// How many bytes.
        len: u32,
        /// Where in the file.
        file_off: u64,
    },
    /// See [`crate::Files::write_at`].
    Write {
        /// The file.
        file: File,
        /// The extent written from.
        extent: Extent,
        /// Where in the extent.
        at: u32,
        /// How many bytes.
        len: u32,
        /// Where in the file.
        file_off: u64,
    },
    /// See [`crate::Files::write_spans`].
    WriteSpans {
        /// The file.
        file: File,
        /// The spans, in order.
        spans: &'a [Span],
        /// Where in the file.
        file_off: u64,
    },
    /// See [`crate::Files::sync`].
    Sync {
        /// The file.
        file: File,
        /// Leave out metadata that reading the data back does not need.
        data_only: bool,
    },
    /// See [`crate::Files::reserve`].
    Reserve {
        /// The file.
        file: File,
        /// Where the range starts.
        off: u64,
        /// Its length; not 0.
        len: u64,
        /// Leave the file's size as it is.
        keep_size: bool,
    },
    /// See [`crate::Files::punch`].
    Punch {
        /// The file.
        file: File,
        /// Where the range starts; whole blocks.
        off: u64,
        /// Its length; whole blocks, not 0.
        len: u64,
    },
    /// See [`crate::Files::status`].
    Status {
        /// The path.
        path: &'a str,
    },
    /// See [`crate::Files::rename`].
    Rename {
        /// The present path.
        from: &'a str,
        /// The new path.
        to: &'a str,
    },
    /// See [`crate::Files::unlink`].
    Unlink {
        /// The path.
        path: &'a str,
    },
    /// See [`crate::Files::mkdir`].
    Mkdir {
        /// The path.
        path: &'a str,
    },
}
