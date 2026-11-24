// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TCP connections over `Io`: a fixed set of connections and listeners,
//! framing, frames lent in place, and time owned by the caller.
//!
//! This crate is being built in stages (OpenSpec change `add-tcp-crate`).
//! The parts here do no I/O: connection handles, framing, reconnect
//! backoff and the deadline table.

pub mod backoff;
pub mod deadline;
pub mod framing;
pub mod handle;

pub use backoff::Backoff;
pub use deadline::Deadlines;
pub use framing::{FrameError, Framing, FramingError, HeaderFraming};
pub use handle::{Conn, Generations, Listener};
