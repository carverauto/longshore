// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What every longshore crate is written against.
//!
//! - [`io`]: the [`io::Io`] trait, its operations and limits, and the
//!   `io_uring` backend. A protocol crate does no I/O of its own: it submits
//!   operations to an `Io` and is fed their completions.
//! - `sim` (feature `sim`): a deterministic simulator that stands in for the
//!   kernel in tests, with virtual sockets, files and time.

pub mod io;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
