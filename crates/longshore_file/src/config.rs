// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The plan: a set's share of the backend (contract C2).

use crate::FileSizes;
use longshore_common::io::Limits;

/// Why a plan was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    /// `ops_per_file` is 0 or above `shared + 1`, or `files` is 0.
    Ops,
    /// `sequence_max` is 0 or above `ops_per_file`.
    Sequence,
    /// `spans_max` is 0 or above 1,024.
    Spans,
    /// The pool's small buffers cannot hold a span list of `spans_max`
    /// spans, or a 256-byte status area and a path.
    ClassSize,
    /// The set's tokens overflow, or a count does.
    Tokens,
}

/// A set's share of the backend: what [`FileConfig::plan`] added.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileConfig {
    pub(crate) sizes: FileSizes,
    pub(crate) token_base: u64,
    pub(crate) file_slot_base: u32,
    pub(crate) small_base: u32,
    pub(crate) path_max: u32,
}

impl FileConfig {
    /// `limits` with room for a set of `sizes`, and the set's share.
    pub fn plan(limits: Limits, sizes: FileSizes) -> Result<(Limits, FileConfig), ConfigError> {
        crate::plan::plan(limits, sizes)
    }

    /// The first backend token of the set.
    pub const fn token_base(&self) -> u64 {
        self.token_base
    }

    /// How many backend tokens the set uses, from [`FileConfig::token_base`].
    pub const fn tokens(&self) -> u64 {
        let s = self.sizes;
        (s.files as u64 + s.paths as u64 + s.shared as u64) * 2
    }

    /// The longest path an operation takes, in bytes.
    pub const fn path_max(&self) -> u32 {
        self.path_max
    }
}
