// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Directory log overflow and cross-directory rename of non-empty directory tests.

use super::super::{base_limits, dir_open, open_path, sync_dir_path};
use super::{
    assert_file_byte, assert_missing, assert_open_reads, create_empty, create_write_sync,
    fail_dir_sync, mkdir_path, ping_pong, rename_path, unlink_path,
};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::ENOENT;
use longshore_common::io::{Completions, FileSlot};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

const CFG: DiskConfig = DiskConfig::BASE
    .with_max_files(8)
    .with_max_dirs(4)
    .with_max_bytes_per_file(16 * 1024)
    .with_max_name_len(128)
    .with_write_log_entries(16)
    .with_write_log_bytes(16 * 1024)
    .with_read_error_sectors(8)
    .with_sector_size(512);

fn sim_with(seed: u64) -> SimIo {
    let mut sim = SimIo::new(base_limits(), seed, Faults::default());
    sim.setup_disk(CFG);
    sim
}

#[test]
fn dir_log_overflow_and_cross_dir_nonempty_rename_under_sync_fail() {
    let mut sim = sim_with(19);
    let mut out = Completions::new(16);
    sealed(|| root_dir_log_overflow_keeps_synced_entries(&mut sim, &mut out));

    sim = sim_with(29);
    sealed(|| cross_dir_rename_of_poisoned_nonempty_dir(&mut sim, &mut out));
}

/// 70 renames overflow the root dir log; synced files survive, unlinked ones stay gone.
fn root_dir_log_overflow_keeps_synced_entries(sim: &mut SimIo, out: &mut Completions) {
    create_write_sync(sim, out, 1, "/keep", 0x21);
    create_empty(sim, out, 5, "/tmp_u");
    unlink_path(sim, out, 7, "/tmp_u");
    create_write_sync(sim, out, 8, "/m0", 0x33);

    ping_pong(sim, out, 100, 70, "/m0", "/m1");
    rename_path(sim, out, 200, "/m0", "/moved_final");
    create_write_sync(sim, out, 201, "/target", 0x44);
    create_empty(sim, out, 205, "/ephem");
    unlink_path(sim, out, 207, "/ephem");

    sync_dir_path(sim, out, 210, "/");
    sim.power_loss();

    assert_file_byte(sim, out, 220, "/keep", 0x21);
    assert_file_byte(sim, out, 223, "/moved_final", 0x33);
    assert_open_reads(sim, out, 226, "/target", 0x44);
    assert_missing(sim, out, 228, "/tmp_u");
    assert_missing(sim, out, 229, "/ephem");
}

/// `/d1/sub` (with a poisoned entry) moves to `/d2` after `/d1`'s log overflows.
fn cross_dir_rename_of_poisoned_nonempty_dir(sim: &mut SimIo, out: &mut Completions) {
    mkdir_path(sim, out, 1, "/d1");
    mkdir_path(sim, out, 2, "/d2");
    sync_dir_path(sim, out, 10, "/");
    mkdir_path(sim, out, 20, "/d1/sub");
    sync_dir_path(sim, out, 30, "/d1");

    create_write_sync(sim, out, 40, "/d1/sub/keep", 0x55);
    sync_dir_path(sim, out, 50, "/d1/sub");

    create_empty(sim, out, 60, "/d1/sub/lost");
    fail_dir_sync(sim, out, 70, "/d1/sub");

    create_empty(sim, out, 80, "/d1/p0");
    ping_pong(sim, out, 100, 70, "/d1/p0", "/d1/p1");

    rename_path(sim, out, 200, "/d1/sub", "/d2/sub");
    sync_dir_path(sim, out, 210, "/d1");
    sync_dir_path(sim, out, 220, "/d2");
    sim.power_loss();

    assert_open_reads(sim, out, 230, "/d2/sub/keep", 0x55);
    assert_missing(sim, out, 232, "/d2/sub/lost");
    assert_eq!(
        open_path(sim, out, 233, FileSlot::new(1), "/d1/sub", dir_open()),
        -ENOENT
    );
}
