// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for ancestor-directory rename ordering and replaced
//! directory subtree ordering in `SimDisk::sync_dir`.

use super::super::{
    base_limits, dir_open, drive, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot,
    write_fill,
};
use super::{fail_dir_sync, mkdir_path, rename_path};
use longshore_common::io::alloc_guard::sealed;
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

fn setup_causal_sim(seed: u64, max_name_len: usize) -> SimIo {
    let mut sim = SimIo::new(base_limits(), seed, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(6)
            .with_max_dirs(8)
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
fn rename_ancestor_shorter_then_create_in_durable_subdir_sync_subdir_first() {
    let mut sim = setup_causal_sim(201, 20);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/aaaaaaaa");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/aaaaaaaa/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/aaaaaaaa");

        rename_path(&mut sim, &mut out, 40, "/aaaaaaaa", "/b");
        create_write_sync_close(&mut sim, &mut out, 50, "/b/sub/1234567", 0xA1);
        sync_dir_path(&mut sim, &mut out, 60, "/b/sub");
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 80, "/b/sub/1234567", 0xA1);
    });
}

#[test]
fn rename_ancestor_shorter_then_mkdir_in_durable_subdir_sync_subdir_first() {
    let mut sim = setup_causal_sim(203, 20);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/aaaaaaaa");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/aaaaaaaa/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/aaaaaaaa");

        rename_path(&mut sim, &mut out, 40, "/aaaaaaaa", "/b");
        mkdir_path(&mut sim, &mut out, 50, "/b/sub/1234567");
        sync_dir_path(&mut sim, &mut out, 60, "/b/sub");
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sim.power_loss();

        assert_dir_exists(&mut sim, &mut out, 80, "/b/sub/1234567");
    });
}

#[test]
fn rename_ancestor_shorter_then_cross_dir_rename_into_durable_subdir() {
    let mut sim = setup_causal_sim(205, 20);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/aaaaaaaa");
        mkdir_path(&mut sim, &mut out, 2, "/p");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/aaaaaaaa/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/aaaaaaaa");
        create_write_sync_close(&mut sim, &mut out, 35, "/p/1234567", 0xB2);
        sync_dir_path(&mut sim, &mut out, 39, "/p");

        rename_path(&mut sim, &mut out, 40, "/aaaaaaaa", "/b");
        rename_path(&mut sim, &mut out, 41, "/p/1234567", "/b/sub/1234567");
        sync_dir_path(&mut sim, &mut out, 50, "/b/sub");
        sync_dir_path(&mut sim, &mut out, 60, "/p");
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 80, "/b/sub/1234567", 0xB2);
    });
}

#[test]
fn rename_child_out_of_dst_then_replace_dst_with_open_dir_sync() {
    let mut sim = setup_causal_sim(207, 64);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/dst");
        mkdir_path(&mut sim, &mut out, 2, "/x");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        create_write_sync_close(&mut sim, &mut out, 20, "/dst/child", 0xC3);
        sync_dir_path(&mut sim, &mut out, 30, "/dst");

        assert_eq!(
            open_path(&mut sim, &mut out, 40, FileSlot::new(0), "/dst", dir_open()),
            0
        );
        rename_path(&mut sim, &mut out, 41, "/dst/child", "/saved");
        rename_path(&mut sim, &mut out, 42, "/x", "/dst");
        sync_dir_path(&mut sim, &mut out, 50, "/");
        assert_eq!(sync_slot(&mut sim, &mut out, 60, FileSlot::new(0)), 0);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                61,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 70, "/saved", 0xC3);
        assert_dir_exists(&mut sim, &mut out, 80, "/dst");
    });
}

#[test]
fn nested_subdir_child_moved_out_before_dst_replaced_survives_power_loss() {
    let mut sim = setup_causal_sim(209, 64);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/dst");
        mkdir_path(&mut sim, &mut out, 2, "/x");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/dst/sub");
        sync_dir_path(&mut sim, &mut out, 25, "/dst");
        create_write_sync_close(&mut sim, &mut out, 30, "/dst/sub/child", 0xD4);
        sync_dir_path(&mut sim, &mut out, 35, "/dst/sub");

        assert_eq!(
            open_path(&mut sim, &mut out, 40, FileSlot::new(0), "/dst", dir_open()),
            0
        );
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                41,
                FileSlot::new(1),
                "/dst/sub",
                dir_open()
            ),
            0
        );
        rename_path(&mut sim, &mut out, 42, "/dst/sub/child", "/saved");
        rename_path(&mut sim, &mut out, 43, "/dst/sub", "/out_sub");
        rename_path(&mut sim, &mut out, 44, "/x", "/dst");
        sync_dir_path(&mut sim, &mut out, 50, "/");
        assert_eq!(sync_slot(&mut sim, &mut out, 60, FileSlot::new(0)), 0);
        assert_eq!(sync_slot(&mut sim, &mut out, 61, FileSlot::new(1)), 0);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                62,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                63,
                Op::FileClose {
                    slot: FileSlot::new(1)
                }
            ),
            0
        );
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 70, "/saved", 0xD4);
        assert_dir_exists(&mut sim, &mut out, 80, "/out_sub");
        assert_dir_exists(&mut sim, &mut out, 85, "/dst");
    });
}

#[test]
fn poisoned_unrelated_dir_rename_does_not_block_later_moved_subdir_sync() {
    let mut sim = setup_causal_sim(211, 64);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/a");
        mkdir_path(&mut sim, &mut out, 2, "/other");
        sync_dir_path(&mut sim, &mut out, 10, "/");

        rename_path(&mut sim, &mut out, 20, "/a", "/b");
        fail_dir_sync(&mut sim, &mut out, 25, "/");

        create_write_sync_close(&mut sim, &mut out, 30, "/other/f", 0xE5);
        rename_path(&mut sim, &mut out, 40, "/other", "/b/other");
        sync_dir_path(&mut sim, &mut out, 50, "/b/other");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 60, "/other/f", 0xE5);
    });
}

#[test]
fn move_durable_subtree_then_move_out_and_lengthen_ancestor_preserves_saved() {
    let mut sim = setup_causal_sim(213, 22);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/y");
        mkdir_path(&mut sim, &mut out, 2, "/z");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/z/leaf");
        sync_dir_path(&mut sim, &mut out, 30, "/z");
        create_write_sync_close(&mut sim, &mut out, 35, "/z/leaf/123456789", 0xF1);
        sync_dir_path(&mut sim, &mut out, 39, "/z/leaf");

        rename_path(&mut sim, &mut out, 40, "/z", "/y/z");
        rename_path(&mut sim, &mut out, 41, "/y/z/leaf/123456789", "/saved");
        rename_path(&mut sim, &mut out, 42, "/y", "/long_dir_name");
        sync_dir_path(&mut sim, &mut out, 50, "/");
        sync_dir_path(&mut sim, &mut out, 60, "/long_dir_name");
        sync_dir_path(&mut sim, &mut out, 70, "/long_dir_name/z/leaf");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 80, "/saved", 0xF1);
    });
}

#[test]
fn move_durable_other_then_create_and_move_out_before_lengthening_ancestor() {
    let mut sim = setup_causal_sim(215, 28);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        mkdir_path(&mut sim, &mut out, 2, "/other");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/other/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/other");

        rename_path(&mut sim, &mut out, 40, "/other", "/d/other");
        create_write_sync_close(&mut sim, &mut out, 45, "/d/other/sub/1234567890123", 0xF2);
        rename_path(
            &mut sim,
            &mut out,
            50,
            "/d/other/sub/1234567890123",
            "/saved",
        );
        rename_path(&mut sim, &mut out, 51, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 60, "/");
        sync_dir_path(&mut sim, &mut out, 70, "/longdir");
        sync_dir_path(&mut sim, &mut out, 80, "/longdir/other/sub");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 90, "/saved", 0xF2);
    });
}

#[test]
fn deep_durable_hierarchy_move_out_before_lengthening_ancestor_exceeds_j_steps() {
    let mut sim = setup_causal_sim(217, 32);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        mkdir_path(&mut sim, &mut out, 2, "/other");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/other/a");
        sync_dir_path(&mut sim, &mut out, 25, "/other");
        mkdir_path(&mut sim, &mut out, 30, "/other/a/b");
        sync_dir_path(&mut sim, &mut out, 35, "/other/a");
        mkdir_path(&mut sim, &mut out, 40, "/other/a/b/c");
        sync_dir_path(&mut sim, &mut out, 45, "/other/a/b");
        mkdir_path(&mut sim, &mut out, 50, "/other/a/b/c/d");
        sync_dir_path(&mut sim, &mut out, 55, "/other/a/b/c");
        create_write_sync_close(&mut sim, &mut out, 60, "/other/a/b/c/d/1234567890123", 0xF3);
        sync_dir_path(&mut sim, &mut out, 65, "/other/a/b/c/d");

        rename_path(&mut sim, &mut out, 70, "/other", "/d/other");
        rename_path(
            &mut sim,
            &mut out,
            71,
            "/d/other/a/b/c/d/1234567890123",
            "/saved",
        );
        rename_path(&mut sim, &mut out, 72, "/d", "/longdir");
        sync_dir_path(&mut sim, &mut out, 80, "/");
        sync_dir_path(&mut sim, &mut out, 90, "/longdir");
        sync_dir_path(&mut sim, &mut out, 100, "/longdir/other/a/b/c/d");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 110, "/saved", 0xF3);
    });
}
