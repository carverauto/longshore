// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-03` backend test entrypoints (`SimIo`, `Posix`, `UringIo`).

use super::super::harness::{posix_backend, TempDir};
use super::parity_fileserver::{parity_limits, run_mini_fileserver};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(feature = "sim")]
use longshore::sim::{DiskConfig, Faults, SimIo};

#[cfg(feature = "sim")]
#[test]
fn t4_03_cross_backend_parity_fileserver_sim() {
    let lim = parity_limits();
    let mut sim = SimIo::new(lim, 303, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 8,
        max_bytes_per_file: 16 * 1024,
        max_name_len: 128,
        write_log_entries: 8,
        write_log_bytes: 8 * 1024,
        read_error_sectors: 4,
        sector_size: 512,
    });
    run_mini_fileserver(
        &mut sim,
        lim,
        "127.0.0.1:9303".parse().unwrap(),
        "/tmp/t403_sim.sock",
        "/t403_sim.dat",
    );
}

#[cfg(unix)]
#[test]
fn t4_03_cross_backend_parity_fileserver_posix() {
    let tmp = TempDir::new("t4_03_posix").unwrap();
    let lim = parity_limits();
    let mut posix = posix_backend(lim).unwrap();
    run_mini_fileserver(
        &mut posix,
        lim,
        "127.0.0.1:0".parse().unwrap(),
        tmp.socket_path("p.sock").to_str().unwrap(),
        tmp.file_path("store.dat").to_str().unwrap(),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn t4_03_cross_backend_parity_fileserver_uring() {
    let tmp = TempDir::new("t4_03_uring").unwrap();
    let lim = parity_limits();
    let mut uring = UringIo::new(lim).unwrap();
    run_mini_fileserver(
        &mut uring,
        lim,
        "127.0.0.1:0".parse().unwrap(),
        tmp.socket_path("u.sock").to_str().unwrap(),
        tmp.file_path("store.dat").to_str().unwrap(),
    );
}
