// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Linux real-kernel `UringIo` E2E workload suite (`#[cfg(target_os = "linux")]`).
//!
//! Covers `IORING_SETUP_SUBMIT_ALL`, `> 16,384` registered buffers (`<= 1 GiB`
//! ranges), `RLIMIT_MEMLOCK` / `RLIMIT_NOFILE` child process diagnostics,
//! `O_DIRECT` chains + `FilePunch` + `statx`, `IORING_FEAT_NODROP` & CQ sizing,
//! `TCP_NODELAY` / `SO_KEEPALIVE` `getsockopt` verification, 1,000 idle
//! connections holding zero ring buffers, and `alloc_guard` steady-state checks.

pub mod ring_direct_chains;
pub mod ring_limits_submit;
pub mod server_ring_scale;
