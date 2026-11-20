// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Opaque-box E2E test suite tree for `longshore` (`#41`–`#45`).
//!
//! All modules import strictly from the public `longshore::...` facade and
//! standard library primitives. Modules requiring `sim` or Linux `io_uring`
//! are feature- and target-gated cleanly.

pub mod harness;

pub mod t12_armed_net;
pub mod t12_file_chains;
pub mod t12_file_ops;
pub mod t12_limits_engine;
pub mod t12_mailbox_core;
pub mod t12_mailbox_s3;
pub mod t12_pool_fileslots;
pub mod t12_s3_portable_uring;
pub mod t12_shared_posix;
#[cfg(feature = "sim")]
pub mod t12_sim_contract;
#[cfg(feature = "sim")]
pub mod t12_sim_disk_durability;
#[cfg(feature = "sim")]
pub mod t12_sim_disk_faults;
pub mod t12_sockets_server;
pub mod t12_stream_tls;

#[cfg(feature = "sim")]
pub mod t3_pairwise_io_disk;
pub mod t3_pairwise_mailbox;
pub mod t3_pairwise_net_tls;

pub mod t4_apps_client_mailbox;
pub mod t4_apps_network;
#[cfg(feature = "sim")]
pub mod t4_apps_storage;

#[cfg(target_os = "linux")]
pub mod linux_uring_suite;
#[cfg(feature = "sim")]
pub mod sealed_suite;
pub mod t5_adversarial;
