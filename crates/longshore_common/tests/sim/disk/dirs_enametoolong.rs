// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for `ENAMETOOLONG` causal ordering in `SimDisk::sync_dir`,
//! over-long durable descendant handling in `apply_durable_rename`, and
//! `evict_one_dir_change` `else`-branch parent inode reclamation.

use super::super::{
    base_limits, dir_open, drive, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot,
    write_fill,
};
use super::{fail_dir_sync, mkdir_path, rename_path, unlink_path};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::{ENAMETOOLONG, ENOENT};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

fn create_write_sync_close(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str, byte: u8) {
    assert_eq!(
        open_path(sim, out, tok, FileSlot::new(0), path, rw_create()),
        0
    );
    assert_eq!(
        write_fill(sim, out, tok + 1, FileSlot::new(0), 0, 16, byte),
        16
    );
    assert_eq!(sync_slot(sim, out, tok + 2, FileSlot::new(0)), 0);
    assert_eq!(
        drive(
            sim,
            out,
            tok + 3,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );
}

fn assert_file_byte(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str, byte: u8) {
    assert_eq!(
        open_path(sim, out, tok, FileSlot::new(0), path, rw_open()),
        0
    );
    assert_eq!(read_at(sim, out, tok + 1, FileSlot::new(0), 0, 16), 16);
    assert!(sim.buf(BufId::new(2))[..16].iter().all(|&b| b == byte));
    assert_eq!(
        drive(
            sim,
            out,
            tok + 2,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );
}

fn assert_dir_exists(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(
        open_path(sim, out, tok, FileSlot::new(0), path, dir_open()),
        0
    );
    assert_eq!(
        drive(
            sim,
            out,
            tok + 1,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );
}

fn setup_enametoolong_sim(seed: u64, max_name_len: usize, max_files: u32) -> SimIo {
    let mut sim = SimIo::new(base_limits(), seed, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(max_files)
            .with_max_dirs(4)
            .with_max_bytes_per_file(4096)
            .with_max_name_len(max_name_len)
            .with_write_log_entries(8)
            .with_write_log_bytes(4096)
            .with_read_error_sectors(4)
            .with_sector_size(512),
    );
    sim
}

#[test]
fn zero_fault_unlink_long_child_then_rename_dir_and_sync_survives_power_loss() {
    let mut sim = setup_enametoolong_sim(101, 16, 1);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        create_write_sync_close(&mut sim, &mut out, 20, "/d/1234567890123", 0xAA);
        sync_dir_path(&mut sim, &mut out, 30, "/d");

        unlink_path(&mut sim, &mut out, 40, "/d/1234567890123");
        rename_path(&mut sim, &mut out, 41, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 50, "/");
        sync_dir_path(&mut sim, &mut out, 60, "/longdir");
        sim.power_loss();

        assert_dir_exists(&mut sim, &mut out, 70, "/longdir");
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                75,
                FileSlot::new(1),
                "/longdir/1234567890123",
                rw_open(),
            ),
            -ENAMETOOLONG
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 76, FileSlot::new(1), "/d", dir_open()),
            -ENOENT
        );
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                77,
                FileSlot::new(1),
                "/longdir/new",
                rw_create(),
            ),
            0
        );
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                78,
                Op::FileClose {
                    slot: FileSlot::new(1)
                }
            ),
            0
        );
    });
}

#[test]
fn zero_fault_rename_long_child_out_then_rename_dir_and_sync_survives_power_loss() {
    let mut sim = setup_enametoolong_sim(103, 16, 4);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        create_write_sync_close(&mut sim, &mut out, 20, "/d/1234567890123", 0xBB);
        sync_dir_path(&mut sim, &mut out, 30, "/d");

        rename_path(&mut sim, &mut out, 40, "/d/1234567890123", "/f");
        rename_path(&mut sim, &mut out, 41, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 50, "/");
        sync_dir_path(&mut sim, &mut out, 60, "/longdir");
        sim.power_loss();

        assert_dir_exists(&mut sim, &mut out, 70, "/longdir");
        assert_file_byte(&mut sim, &mut out, 80, "/f", 0xBB);
    });
}

#[test]
fn zero_fault_nested_subdir_long_child_unlink_or_rename_then_rename_dir_survives() {
    // Variant 1: Unlink nested child under /d/sub before Rename("/d", "/longdir").
    let mut sim = setup_enametoolong_sim(107, 18, 4);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/d/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/d");
        create_write_sync_close(&mut sim, &mut out, 40, "/d/sub/12345678901", 0xCC);
        sync_dir_path(&mut sim, &mut out, 50, "/d/sub");

        unlink_path(&mut sim, &mut out, 60, "/d/sub/12345678901");
        rename_path(&mut sim, &mut out, 61, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sync_dir_path(&mut sim, &mut out, 80, "/longdir/sub");
        sim.power_loss();

        assert_dir_exists(&mut sim, &mut out, 90, "/longdir/sub");
    });

    // Variant 2: Move nested child out to /f before Rename("/d", "/longdir").
    sim = setup_enametoolong_sim(109, 18, 4);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/d/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/d");
        create_write_sync_close(&mut sim, &mut out, 40, "/d/sub/12345678901", 0xDD);
        sync_dir_path(&mut sim, &mut out, 50, "/d/sub");

        rename_path(&mut sim, &mut out, 60, "/d/sub/12345678901", "/f");
        rename_path(&mut sim, &mut out, 61, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sync_dir_path(&mut sim, &mut out, 80, "/longdir/sub");
        sim.power_loss();

        assert_dir_exists(&mut sim, &mut out, 90, "/longdir/sub");
        assert_file_byte(&mut sim, &mut out, 100, "/f", 0xDD);
    });
}

#[test]
fn poisoned_or_unsynced_unlink_long_child_then_rename_dir_and_sync_root_survives() {
    const SLOT_1: FileSlot = FileSlot::new(1);
    let mut sim = setup_enametoolong_sim(113, 16, 2);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        create_write_sync_close(&mut sim, &mut out, 20, "/d/keep", 0xAB);
        create_write_sync_close(&mut sim, &mut out, 25, "/d/1234567890123", 0xEE);
        sync_dir_path(&mut sim, &mut out, 30, "/d");

        unlink_path(&mut sim, &mut out, 40, "/d/1234567890123");
        fail_dir_sync(&mut sim, &mut out, 50, "/d");

        rename_path(&mut sim, &mut out, 60, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sim.power_loss();

        assert_dir_exists(&mut sim, &mut out, 80, "/longdir");
        assert_file_byte(&mut sim, &mut out, 85, "/longdir/keep", 0xAB);
        // Dropped over-long child's inode and data_used slot (max_files = 2) must be freed.
        assert_eq!(
            open_path(&mut sim, &mut out, 90, SLOT_1, "/new", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 91, Op::FileClose { slot: SLOT_1 }),
            0
        );
    });

    // Unsynced Unlink("/d/1234567890123") settled via power_loss across multiple seeds.
    for seed in 131..147u64 {
        sim = setup_enametoolong_sim(seed, 16, 1);
        sealed(|| {
            mkdir_path(&mut sim, &mut out, 1, "/d");
            sync_dir_path(&mut sim, &mut out, 10, "/");
            create_write_sync_close(&mut sim, &mut out, 20, "/d/1234567890123", 0xEF);
            sync_dir_path(&mut sim, &mut out, 30, "/d");

            unlink_path(&mut sim, &mut out, 40, "/d/1234567890123");
            rename_path(&mut sim, &mut out, 41, "/d", "/longdir");
            sync_dir_path(&mut sim, &mut out, 50, "/");
            sim.power_loss();

            assert_dir_exists(&mut sim, &mut out, 60, "/longdir");
            assert_eq!(
                open_path(&mut sim, &mut out, 70, SLOT_1, "/new", rw_create()),
                0
            );
            assert_eq!(
                drive(&mut sim, &mut out, 71, Op::FileClose { slot: SLOT_1 }),
                0
            );
        });
    }
}

#[test]
fn evict_one_dir_change_else_branch_reclaims_parent_dir_inode() {
    let mut sim = SimIo::new(base_limits(), 127, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(1)
            .with_max_dirs(2)
            .with_max_bytes_per_file(4096)
            .with_max_name_len(64)
            .with_write_log_entries(8)
            .with_write_log_bytes(4096)
            .with_read_error_sectors(4)
            .with_sector_size(512),
    );
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        mkdir_path(&mut sim, &mut out, 2, "/d2");
        sync_dir_path(&mut sim, &mut out, 10, "/");

        create_write_sync_close(&mut sim, &mut out, 20, "/d1/f", 0xAA);
        sync_dir_path(&mut sim, &mut out, 30, "/d1");

        unlink_path(&mut sim, &mut out, 40, "/d1/f");
        fail_dir_sync(&mut sim, &mut out, 50, "/d1");

        rename_path(&mut sim, &mut out, 60, "/d2", "/d1");
        sync_dir_path(&mut sim, &mut out, 70, "/");

        rename_path(&mut sim, &mut out, 80, "/d1", "/z");
        fail_dir_sync(&mut sim, &mut out, 90, "/");

        for i in 0..70u64 {
            let (old_p, new_p) = if i % 2 == 0 {
                ("/z", "/d1")
            } else {
                ("/d1", "/z")
            };
            rename_path(&mut sim, &mut out, 100 + i, old_p, new_p);
        }

        mkdir_path(&mut sim, &mut out, 200, "/extra");
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                210,
                FileSlot::new(0),
                "/file1",
                rw_create()
            ),
            0
        );
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                211,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
    });
}
