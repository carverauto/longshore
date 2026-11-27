// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The contract suite of `Files`, shipped in the library so that every
//! backend runs the same checks: the simulator and the portable backend
//! from this crate's tests, the ring from `longshore-bench`'s `files_uring`
//! on a real kernel.
//!
//! [`run_all`] checks every operation of contract C5 and its result, whole
//! transfers, ordered sequences, direct-I/O refusals, stale handles, the
//! per-file bound, path slots, work that waits for room and a held-back
//! close. Poisoning needs a backend that can be made to fail a sync, which
//! only the simulator can: the model test and the simulator's own tests
//! check it.
//!
//! [`Rig`] is a set on a backend wrapped in a [`Spy`], which records what
//! the set submits and can refuse it room, with the caller's loop around
//! them; the model, property and allocation tests build on it.
//!
//! # Panics
//!
//! This module is test code shipped in the library, and the only part of
//! the crate that panics. A check that fails stops the thread through
//! [`fail`], which unwinds with a message naming the place and the reason,
//! as a failed assertion does; [`must`], [`some`], [`run_all`] and the
//! methods of [`Rig`] fail that way. [`Spy`] never does. Nothing in
//! [`Files`](crate::Files) or the rest of the crate calls into this module,
//! so a program that only uses the set never reaches a panic; it should
//! call `check` only from a test or a self-check such as `files_uring`.

use core::fmt;
use longshore_common::io::{Io, Limits, OpenFlags};

mod basics;
mod bounds;
mod closing;
mod direct;
mod kind;
mod refusals;
mod rig;
mod sequences;
mod spy;
mod transfers;

pub use kind::{describe, Kind};
pub use rig::{ticket, Rig};
pub use spy::{Spy, Submitted};

use crate::FileSizes;

/// Stop the running check with `why`. A check is a test: stopping it with a
/// message is how it fails. The message goes to standard error with the
/// place of the call, and the thread unwinds with it, as a failed
/// assertion does.
///
/// # Panics
///
/// Always: it unwinds with the message (`std::panic::resume_unwind`), the
/// one panic of the crate, which every other failing check goes through.
#[track_caller]
pub fn fail(why: fmt::Arguments<'_>) -> ! {
    let at = core::panic::Location::caller();
    // A check that fails inside a sealed region may still say why.
    let payload = longshore_common::io::alloc_guard::allow_alloc(|| {
        let msg = format!("{at}: {why}");
        eprintln!("check failed at {msg}");
        Box::new(msg) as Box<dyn core::any::Any + Send>
    });
    std::panic::resume_unwind(payload)
}

/// The value a check needs, or fail naming `what`.
///
/// # Panics
///
/// Through [`fail`], when `value` is an error.
#[track_caller]
pub fn must<V, E: fmt::Debug>(value: Result<V, E>, what: &str) -> V {
    match value {
        Ok(v) => v,
        Err(e) => fail(format_args!("{what}: {e:?}")),
    }
}

/// The value a check needs to be there, or fail naming `what`.
///
/// # Panics
///
/// Through [`fail`], when `value` is `None`.
#[track_caller]
pub fn some<V>(value: Option<V>, what: &str) -> V {
    match value {
        Some(v) => v,
        None => fail(format_args!("{what}: missing")),
    }
}

/// Block size of every check: the backends' and the extents'.
pub const BLOCK: u32 = 4096;
/// Bytes of an extent in the checks.
pub const EXTENT: u32 = 4 * BLOCK;

/// The backend the checks plan their sets into: one large buffer (an
/// extent of the wrong class for direct I/O), small buffers of 1 KiB for
/// the set's paths and span lists, eight block-aligned extents.
pub fn limits() -> Limits {
    Limits::BASE
        .with_buffers(1)
        .with_buffer_size(EXTENT)
        .with_small_buffer_size(1024)
        .with_aligned_buffers(8)
        .with_aligned_buffer_size(EXTENT)
        .with_block_size(BLOCK)
}

/// The sizes most checks use: two files of three operations each, two
/// shared slots, two path slots, sequences of three, four spans.
pub fn sizes() -> FileSizes {
    FileSizes::new(2, 3)
        .with_shared(2)
        .with_paths(2)
        .with_sequence_max(3)
        .with_spans_max(4)
}

/// Read and write, creating the file if it is missing.
pub const fn rw_create() -> OpenFlags {
    OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true)
}

/// Byte `at` of the pattern numbered `seed`.
pub const fn pattern(seed: u8, at: usize) -> u8 {
    (at as u8).wrapping_mul(31) ^ seed ^ ((at >> 8) as u8)
}

/// `len` bytes of pattern `seed`.
pub fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|at| pattern(seed, at)).collect()
}

/// Run every check of the contract on backends `make` builds, with files
/// under `root` (a directory path ending in `/`, which must exist and be
/// empty enough for the checks' names).
///
/// # Panics
///
/// Through [`fail`], at the first check the backend does not pass, with
/// the place and the reason; a backend that passes every check returns.
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) -> Ran {
    basics::run_all(make, root);
    transfers::run_all(make, root);
    sequences::run_all(make, root);
    refusals::run_all(make, root);
    bounds::run_all(make, root);
    closing::run_all(make, root);
    Ran {
        direct: direct::run_all(make, root),
    }
}

/// What [`run_all`] could check on a backend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ran {
    direct: bool,
}

impl Ran {
    /// Whether the backend opened a file for direct I/O, so that the
    /// direct-I/O checks ran. When it did not, they were left out, and a
    /// message on standard error said so.
    pub const fn direct(&self) -> bool {
        self.direct
    }
}
