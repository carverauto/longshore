// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How large a set is: what [`crate::FileConfig::plan`] adds to `Limits`
//! (contract C2).

/// The counts a set is planned with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileSizes {
    pub(crate) files: u32,
    pub(crate) ops_per_file: u32,
    pub(crate) shared: u32,
    pub(crate) paths: u32,
    pub(crate) sequence_max: u32,
    pub(crate) spans_max: u32,
    pub(crate) token_base: u64,
}

impl FileSizes {
    /// `files` open at once, each with up to `ops_per_file` operations at
    /// once. Defaults: `files` shared operation slots, 4 path slots,
    /// sequences of up to `ops_per_file` steps, 64 spans a gathered write,
    /// tokens from 0.
    pub const fn new(files: u32, ops_per_file: u32) -> FileSizes {
        FileSizes {
            files,
            ops_per_file,
            shared: files,
            paths: 4,
            sequence_max: ops_per_file,
            spans_max: 64,
            token_base: 0,
        }
    }

    /// Operation slots shared by every file, beyond each file's own.
    #[must_use]
    pub const fn with_shared(mut self, shared: u32) -> FileSizes {
        self.shared = shared;
        self
    }

    /// Path operations at once (status, rename, unlink, mkdir), each with
    /// an operation slot and a path buffer of its own.
    #[must_use]
    pub const fn with_paths(mut self, paths: u32) -> FileSizes {
        self.paths = paths;
        self
    }

    /// Steps of one ordered sequence at most; not above `ops_per_file`.
    #[must_use]
    pub const fn with_sequence_max(mut self, steps: u32) -> FileSizes {
        self.sequence_max = steps;
        self
    }

    /// Spans of one gathered write at most; 1 to 1,024.
    #[must_use]
    pub const fn with_spans_max(mut self, spans: u32) -> FileSizes {
        self.spans_max = spans;
        self
    }

    /// The first backend token the set uses; it takes
    /// [`crate::FileConfig::tokens`] from there.
    #[must_use]
    pub const fn with_token_base(mut self, base: u64) -> FileSizes {
        self.token_base = base;
        self
    }
}
