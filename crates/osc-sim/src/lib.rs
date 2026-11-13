// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Deterministic simulation of the I/O layer.
//!
//! [`SimIo`] implements `osc_io::Io` with virtual sockets and a virtual clock.
//! Every scheduling and fault decision comes from one seed: no threads, no
//! wall-clock time, no OS randomness, no hash maps. A client and its test
//! server run on the same `SimIo`.

pub mod prng;
pub mod scenario;
mod sim;

pub use sim::{Faults, SimIo};
