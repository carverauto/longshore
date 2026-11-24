// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared opaque-box harness helpers for the `longshore` E2E test suite.

pub mod http_store;
pub mod io_driver;
pub mod mock_world;
pub mod wal_engine;

pub use http_store::HttpStoreServer;
#[cfg(feature = "tls-core")]
pub use http_store::{make_tls_identity, TestIdentity};
#[cfg(target_os = "linux")]
pub use io_driver::file_uring_bin;
#[cfg(unix)]
pub use io_driver::posix_backend;
pub use io_driver::{
    drain_completions, drive_until, drive_until_nonblocking, read_pool_bytes, read_pool_into,
    submit_and_wait_one, unused_loopback_addr, workspace_root, write_nul_path, write_pool_bytes,
    TempDir,
};
pub use mock_world::{HookCall, OpKind, TestWorld};
pub use wal_engine::{WalConfig, WalEngine};
