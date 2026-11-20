// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Features 11 & 13 (`#42`):
//! - F11: 11 file `Op` variants & `statx` decoder
//! - F13: `file_uring` real-kernel check & direct-IO bench wiring

mod helpers;

pub mod f11_ops_t1a;
pub mod f11_ops_t1b;
pub mod f11_ops_t2;
pub mod f13_uring_t1;
pub mod f13_uring_t2;
