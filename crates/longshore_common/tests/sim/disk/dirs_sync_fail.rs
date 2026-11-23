// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for `SimDisk::poison_dir` preserving already-synced entries.

use super::super::{
    drive, make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot, write_fill,
};
use super::{fail_dir_sync, mkdir_path, rename_path, unlink_path};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::ENOENT;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};

#[test]
fn recreate_same_name_after_poisoned_create_survives_later_dir_sync_fail() {
    let mut sim = make_sim(41, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot(0), "/x", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 2, FileSlot(0), 0, 16, 0xAA),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 3, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 4, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 10, "/");

        unlink_path(&mut sim, &mut out, 20, "/x");
        assert_eq!(
            open_path(&mut sim, &mut out, 21, FileSlot(0), "/x", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 22, FileSlot(0), 0, 16, 0xBB),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 23, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 24, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 30, "/");

        fail_dir_sync(&mut sim, &mut out, 40, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 50, FileSlot(0), "/x", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 51, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0xBB));
    });
}

#[test]
fn synced_child_in_unsynced_dir_survives_later_child_dir_sync_fail() {
    let mut sim = make_sim(43, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        assert_eq!(
            open_path(&mut sim, &mut out, 2, FileSlot(0), "/d1/keep", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 3, FileSlot(0), 0, 16, 0xCC),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 4, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 5, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 10, "/d1");

        assert_eq!(
            open_path(&mut sim, &mut out, 20, FileSlot(0), "/d1/drop", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 21, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 30, "/d1");
        sync_dir_path(&mut sim, &mut out, 40, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 50, FileSlot(0), "/d1/keep", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 51, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0xCC));
        assert_eq!(
            open_path(&mut sim, &mut out, 52, FileSlot(1), "/d1/drop", rw_open()),
            -ENOENT
        );
    });
}

#[test]
fn cross_dir_rename_synced_dirs_and_asymmetric_sync_fail() {
    let mut sim = make_sim(47, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        assert_eq!(
            open_path(&mut sim, &mut out, 20, FileSlot(0), "/d1/f", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 21, FileSlot(0), 0, 16, 0xDD),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 22, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 23, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 30, "/d1");

        mkdir_path(&mut sim, &mut out, 40, "/d2");
        rename_path(&mut sim, &mut out, 41, "/d1/f", "/d2/f");
        sync_dir_path(&mut sim, &mut out, 50, "/d1");
        sync_dir_path(&mut sim, &mut out, 60, "/d2");

        fail_dir_sync(&mut sim, &mut out, 70, "/d1");
        fail_dir_sync(&mut sim, &mut out, 80, "/d2");
        sync_dir_path(&mut sim, &mut out, 90, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 100, FileSlot(0), "/d2/f", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 101, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0xDD));
        assert_eq!(
            open_path(&mut sim, &mut out, 102, FileSlot(1), "/d1/f", rw_open()),
            -ENOENT
        );
    });

    // Asymmetric case: only /d1 synced; /d1 sync_fail must not poison it, while /d2 sync_fail must.
    sim = make_sim(53, 512, 16, 16 * 1024);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        mkdir_path(&mut sim, &mut out, 2, "/d2");
        sync_dir_path(&mut sim, &mut out, 10, "/");

        for (tok_base, name, byte) in [(20u64, "/d1/f1", 0xE1u8), (30, "/d1/f2", 0xE2)] {
            assert_eq!(
                open_path(&mut sim, &mut out, tok_base, FileSlot(0), name, rw_create()),
                0
            );
            assert_eq!(
                write_fill(&mut sim, &mut out, tok_base + 1, FileSlot(0), 0, 16, byte),
                16
            );
            assert_eq!(sync_slot(&mut sim, &mut out, tok_base + 2, FileSlot(0)), 0);
            assert_eq!(
                drive(
                    &mut sim,
                    &mut out,
                    tok_base + 3,
                    Op::FileClose { slot: FileSlot(0) }
                ),
                0
            );
        }
        sync_dir_path(&mut sim, &mut out, 40, "/d1");

        rename_path(&mut sim, &mut out, 50, "/d1/f1", "/d2/f1");
        sync_dir_path(&mut sim, &mut out, 51, "/d1");
        fail_dir_sync(&mut sim, &mut out, 60, "/d1");
        sync_dir_path(&mut sim, &mut out, 70, "/d2");

        rename_path(&mut sim, &mut out, 80, "/d1/f2", "/d2/f2");
        sync_dir_path(&mut sim, &mut out, 81, "/d1");
        fail_dir_sync(&mut sim, &mut out, 90, "/d2");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 100, FileSlot(0), "/d2/f1", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 101, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0xE1));
        assert_eq!(
            drive(&mut sim, &mut out, 102, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 103, FileSlot(1), "/d1/f1", rw_open()),
            -ENOENT
        );

        assert_eq!(
            open_path(&mut sim, &mut out, 110, FileSlot(1), "/d2/f2", rw_open()),
            -ENOENT
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 111, FileSlot(0), "/d1/f2", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 112, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0xE2));
    });
}
