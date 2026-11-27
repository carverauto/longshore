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

pub mod check;
mod config;
mod error;
mod event;
mod files;
mod handle;
mod plan;
mod sizes;
mod span;

pub use config::{ConfigError, FileConfig};
pub use error::{ErrorKind, FileError};
pub use event::{Event, FileUsage};
pub use files::{Files, Range};
pub use handle::{File, Ticket};
pub use sizes::FileSizes;
pub use span::{Span, Step};
