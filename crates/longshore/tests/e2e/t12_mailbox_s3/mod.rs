// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Features 26 (mailbox loom checks, multi-core
//! `SimIo` deterministic replay, `test-model.sh`) and 27 (`longshore_s3::client`
//! rebuilt on `longshore_mailbox`).

mod f26_mailbox_sim_t1;
mod f26_mailbox_sim_t2;
mod f27_s3_client_mailbox;
