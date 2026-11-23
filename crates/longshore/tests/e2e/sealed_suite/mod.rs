// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `alloc_guard::Guard`-sealed E2E suites verifying zero post-setup heap
//! allocation across `Posix` echo, `SimDisk` WAL recovery, armed net buffer
//! rings, `longshore_mailbox` queue/waiters, and threaded `longshore_s3::client`.

pub mod mailbox_s3;
pub mod posix_net_sealed;
pub mod simdisk_wal_sealed;
