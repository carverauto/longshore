// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Features 16 & 17 (`#43`):
//! - F16: `DiskFaults` (11 ppm rates), placed faults, medium bit flip, fault counters
//! - F17: Ported `SimStorage` durability tests, `WalEngine` recovery, and `NOTICE`

mod helpers;

pub mod f16_faults_t1a;
pub mod f16_faults_t1b;
pub mod f16_faults_t2;
pub mod f17_wal_t1;
pub mod f17_wal_t2;
