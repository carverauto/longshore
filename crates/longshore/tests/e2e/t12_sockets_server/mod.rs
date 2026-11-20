// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Features 22 (shutdown-before-close, socket
//! options, Unix-domain sockets) and 23 (shared server networking suite,
//! sim faults, real-kernel server checks).

mod f22_sockets_unix_t1;
mod f22_sockets_unix_t2;
mod f23_server_shared_t1a;
mod f23_server_shared_t1b;
mod f23_server_shared_t2;
