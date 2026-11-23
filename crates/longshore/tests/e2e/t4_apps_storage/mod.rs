// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 4 real-world storage application workloads (`T4-01`, `T4-02`,
//! `T4-07`, `T4-11`, `T4-12`).
//!
//! Gated `#[cfg(feature = "sim")]` in `e2e/mod.rs`.

pub mod direct_io_punch;
pub mod dual_chaos;
mod partitioned_disk;
pub mod partitioned_service;
pub mod wal_lifecycle;
mod wal_stack;
pub mod writeback_as_eio;
