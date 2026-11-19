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

/// Runs all sealed E2E checks in sequence. Intended for dedicated
/// `#[global_allocator] static ALLOC: longshore::io::alloc_guard::Guard = ...`
/// test binaries as well as individual `#[test]` execution.
#[allow(dead_code)]
pub fn run_all_sealed() {
    #[cfg(unix)]
    posix_net_sealed::run_posix_echo_sealed();
    simdisk_wal_sealed::run_simdisk_wal_recovery_sealed();
    posix_net_sealed::run_armed_net_ring_sealed();
    mailbox_s3::run_mailbox_queue_and_waiters_sealed();
    mailbox_s3::run_threaded_s3_client_sealed();
}
