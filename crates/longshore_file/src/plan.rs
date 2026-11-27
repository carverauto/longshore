// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The plan's arithmetic (contract C2): what a set adds to `Limits`, and
//! what it refuses.

use crate::{ConfigError, FileConfig, FileSizes};
use longshore_common::io::Limits;

/// Bytes of a path operation's buffer that hold the status of `status`:
/// a Linux `struct statx`.
pub(crate) const STATUS_AREA: u32 = 256;

/// Bytes of one span in a gathered write's list.
pub(crate) const SPAN_BYTES: u32 = 12;

/// The most spans a gathered write can have.
const SPANS_MOST: u32 = Limits::GATHER_SPANS_MOST;

/// What `sizes` refuses on its own, before the backend's numbers.
fn check(s: &FileSizes) -> Result<(), ConfigError> {
    if s.files == 0 || s.ops_per_file == 0 || s.ops_per_file > s.shared.saturating_add(1) {
        return Err(ConfigError::Ops);
    }
    if s.sequence_max == 0 || s.sequence_max > s.ops_per_file {
        return Err(ConfigError::Sequence);
    }
    if s.spans_max == 0 || s.spans_max > SPANS_MOST {
        return Err(ConfigError::Spans);
    }
    Ok(())
}

/// The longest path a small buffer of `size` bytes holds twice, behind the
/// status area, each with its NUL; 0 when not even one byte fits.
pub(crate) const fn path_max(size: u32) -> u32 {
    match size.checked_sub(STATUS_AREA) {
        Some(rest) => (rest / 2).saturating_sub(1),
        None => 0,
    }
}

pub(crate) fn plan(limits: Limits, sizes: FileSizes) -> Result<(Limits, FileConfig), ConfigError> {
    let s = sizes;
    check(&s)?;
    let ops = s
        .files
        .checked_add(s.paths)
        .and_then(|n| n.checked_add(s.shared))
        .ok_or(ConfigError::Tokens)?;
    // Every record, the file slots' own among them, has an index below
    // `u32::MAX`, which names none (`Ticket::NONE`).
    if ops.checked_add(s.files).is_none_or(|n| n == u32::MAX) {
        return Err(ConfigError::Tokens);
    }
    let small_size = limits.small_buffer_size();
    let path_max = path_max(small_size);
    if path_max == 0 || u64::from(small_size) < u64::from(s.spans_max) * u64::from(SPAN_BYTES) {
        return Err(ConfigError::ClassSize);
    }
    // One token an operation record: the operation slots, then one a file
    // slot for its open and close.
    let tokens = u64::from(ops) + u64::from(s.files);
    let _: u64 = s
        .token_base
        .checked_add(tokens)
        .ok_or(ConfigError::Tokens)?;
    let add = |have: u32, more: u32| have.checked_add(more).ok_or(ConfigError::Tokens);
    let small_more = add(s.files, s.paths)
        .and_then(|n| add(n, ops))
        .and_then(|n| add(limits.small_buffers(), n))
        .map_err(|_| ConfigError::ClassSize)?;
    let out = limits
        .with_ops_max(add(limits.ops_max(), ops)?)
        .with_files_max(add(limits.files_max(), s.files)?)
        .with_gather(
            add(limits.gather_ops(), ops)?,
            limits.gather_spans().max(s.spans_max),
        )
        .with_small_buffers(small_more);
    if !buffers_fit(&out) {
        return Err(ConfigError::ClassSize);
    }
    let config = FileConfig {
        sizes,
        token_base: s.token_base,
        file_slot_base: limits.files_max(),
        small_base: limits.small_buffers(),
        path_max,
    };
    Ok((out, config))
}

/// Whether every buffer of `l` can be named by a `u32`, as `Limits`
/// requires.
fn buffers_fit(l: &Limits) -> bool {
    l.buffers()
        .checked_add(l.small_buffers())
        .and_then(|n| n.checked_add(l.aligned_buffers()))
        .and_then(|n| n.checked_add(l.ring_buffers()))
        .and_then(|n| n.checked_add(l.area_pages()))
        .is_some()
}
