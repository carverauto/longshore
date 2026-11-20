// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Features 5 (shared `Io` contract & `test-linux.sh`)
//! and 6 (`Posix` `kqueue` / `epoll` world).

pub mod f05_shared_contract;
pub mod f05_shared_contract_t2;
pub mod f06_posix_world;
pub mod f06_posix_world_t2;
