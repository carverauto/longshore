// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Features 9–10 (`#42`):
//! - F09: Aligned buffer pool & `<= 1 GiB` range registration
//! - F10: `FileSlot`, `OpenFlags`, `io::files`, and ready-completion queue

mod helpers;

pub mod f09_pool_t1;
pub mod f09_pool_t2;
pub mod f10_slots_t1;
pub mod f10_slots_t2;
