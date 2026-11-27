// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! File I/O over `Io`: a fixed set of files and operations, driven from the
//! caller's loop, on io_uring, the portable backend and the simulator.
//!
//! The set does no I/O itself and never waits. It is file I/O and nothing
//! built on it: logs, pages, records and checksums are left to the program
//! that uses it. The contract it keeps is in the OpenSpec change
//! `add-file-crate` (`design.md`, "The contract").
//!
//! # Panics
//!
//! Nothing a program calls on [`Files`] panics. [`check`], the contract
//! suite shipped so that a real kernel can run it, is test code and the
//! only part of the crate that panics: a failed check unwinds with its
//! place and reason. `Files` never calls it.

mod accept;
mod buffers;
pub mod check;
mod complete;
mod config;
mod errno;
mod error;
mod event;
mod events;
mod files;
mod handle;
mod plan;
mod sequence;
mod sizes;
mod span;
mod state;
mod submit;

pub use config::{ConfigError, FileConfig};
pub use error::{ErrorKind, FileError};
pub use event::{Event, FileUsage};
pub use files::{Files, Range};
pub use handle::{File, Ticket};
pub use sizes::FileSizes;
pub use span::{Span, Step};
