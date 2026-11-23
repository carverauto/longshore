// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for `SimDisk::poison_dir` preserving already-synced entries.

use super::super::{
    drive, make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot, write_fill,
};
use super::{
    assert_file_byte, assert_missing, assert_open_reads, create_write_sync, fail_dir_sync,
    mkdir_path, rename_path, unlink_path,
};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::ENOENT;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::SimIo;

#[test]
fn recreate_same_name_after_poisoned_create_survives_later_dir_sync_fail() {
    let mut sim = make_sim(41, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot::new(0), "/x", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 2, FileSlot::new(0), 0, 16, 0xAA),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 3, FileSlot::new(0)), 0);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                4,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 10, "/");

        unlink_path(&mut sim, &mut out, 20, "/x");
        assert_eq!(
            open_path(&mut sim, &mut out, 21, FileSlot::new(0), "/x", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 22, FileSlot::new(0), 0, 16, 0xBB),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 23, FileSlot::new(0)), 0);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                24,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
        sync_dir_path(&mut sim, &mut out, 30, "/");

        fail_dir_sync(&mut sim, &mut out, 40, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 50, FileSlot::new(0), "/x", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 51, FileSlot::new(0), 0, 16), 16);
        assert!(sim.buf(BufId::new(2))[..16].iter().all(|&b| b == 0xBB));
    });
}

#[test]
fn synced_child_in_unsynced_dir_survives_later_child_dir_sync_fail() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let mut sim = make_sim(43, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        assert_eq!(
            open_path(&mut sim, &mut out, 2, SLOT_0, "/d1/keep", rw_create()),
            0
        );
        assert_eq!(write_fill(&mut sim, &mut out, 3, SLOT_0, 0, 16, 0xCC), 16);
        assert_eq!(sync_slot(&mut sim, &mut out, 4, SLOT_0), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 5, Op::FileClose { slot: SLOT_0 }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 10, "/d1");

        assert_eq!(
            open_path(&mut sim, &mut out, 20, SLOT_0, "/d1/drop", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 21, Op::FileClose { slot: SLOT_0 }),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 30, "/d1");
        sync_dir_path(&mut sim, &mut out, 40, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 50, SLOT_0, "/d1/keep", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 51, SLOT_0, 0, 16), 16);
        assert!(sim.buf(BufId::new(2))[..16].iter().all(|&b| b == 0xCC));
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                52,
                FileSlot::new(1),
                "/d1/drop",
                rw_open()
            ),
            -ENOENT
        );
    });
}

#[test]
fn cross_dir_rename_synced_dirs_and_asymmetric_sync_fail() {
    let mut sim = make_sim(47, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| synced_cross_dir_rename_survives_both_dirs_failing(&mut sim, &mut out));

    // Asymmetric case: only /d1 synced; /d1 sync_fail must not poison it, while /d2 sync_fail must.
    sim = make_sim(53, 512, 16, 16 * 1024);
    sealed(|| asymmetric_sync_fail_poisons_only_unsynced_dir(&mut sim, &mut out));
}

fn synced_cross_dir_rename_survives_both_dirs_failing(sim: &mut SimIo, out: &mut Completions) {
    mkdir_path(sim, out, 1, "/d1");
    sync_dir_path(sim, out, 10, "/");
    create_write_sync(sim, out, 20, "/d1/f", 0xDD);
    sync_dir_path(sim, out, 30, "/d1");

    mkdir_path(sim, out, 40, "/d2");
    rename_path(sim, out, 41, "/d1/f", "/d2/f");
    sync_dir_path(sim, out, 50, "/d1");
    sync_dir_path(sim, out, 60, "/d2");

    fail_dir_sync(sim, out, 70, "/d1");
    fail_dir_sync(sim, out, 80, "/d2");
    sync_dir_path(sim, out, 90, "/");
    sim.power_loss();

    assert_open_reads(sim, out, 100, "/d2/f", 0xDD);
    assert_missing(sim, out, 102, "/d1/f");
}

fn asymmetric_sync_fail_poisons_only_unsynced_dir(sim: &mut SimIo, out: &mut Completions) {
    mkdir_path(sim, out, 1, "/d1");
    mkdir_path(sim, out, 2, "/d2");
    sync_dir_path(sim, out, 10, "/");

    create_write_sync(sim, out, 20, "/d1/f1", 0xE1);
    create_write_sync(sim, out, 30, "/d1/f2", 0xE2);
    sync_dir_path(sim, out, 40, "/d1");

    rename_path(sim, out, 50, "/d1/f1", "/d2/f1");
    sync_dir_path(sim, out, 51, "/d1");
    fail_dir_sync(sim, out, 60, "/d1");
    sync_dir_path(sim, out, 70, "/d2");

    rename_path(sim, out, 80, "/d1/f2", "/d2/f2");
    sync_dir_path(sim, out, 81, "/d1");
    fail_dir_sync(sim, out, 90, "/d2");
    sim.power_loss();

    assert_file_byte(sim, out, 100, "/d2/f1", 0xE1);
    assert_missing(sim, out, 103, "/d1/f1");

    assert_missing(sim, out, 110, "/d2/f2");
    assert_open_reads(sim, out, 111, "/d1/f2", 0xE2);
}
