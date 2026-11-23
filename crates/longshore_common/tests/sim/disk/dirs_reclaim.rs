// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for durable entry replacement and inode reclamation after
//! poisoned `Unlink`/`Rename` and `dir_log` overflow.

use super::super::{
    base_limits, dir_open, drive, make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path,
    sync_slot, write_fill,
};
use super::{fail_dir_sync, mkdir_path, rename_path, unlink_path};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::{ENOENT, ENOTDIR};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

fn create_write_sync(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str, byte: u8) {
    assert_eq!(open_path(sim, out, tok, FileSlot(0), path, rw_create()), 0);
    assert_eq!(write_fill(sim, out, tok + 1, FileSlot(0), 0, 16, byte), 16);
    assert_eq!(sync_slot(sim, out, tok + 2, FileSlot(0)), 0);
    assert_eq!(
        drive(sim, out, tok + 3, Op::FileClose { slot: FileSlot(0) }),
        0
    );
}

fn assert_file_byte(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str, byte: u8) {
    assert_eq!(open_path(sim, out, tok, FileSlot(0), path, rw_open()), 0);
    assert_eq!(read_at(sim, out, tok + 1, FileSlot(0), 0, 16), 16);
    assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == byte));
    assert_eq!(
        drive(sim, out, tok + 2, Op::FileClose { slot: FileSlot(0) }),
        0
    );
}

#[test]
fn poisoned_unlink_then_recreate_file_and_sync_survives_power_loss() {
    for (seed, overflow_before_sync, overflow_after_sync) in
        [(61u64, false, false), (62, false, true), (63, true, false)]
    {
        let mut sim = make_sim(seed, 512, 16, 16 * 1024);
        let mut out = Completions::new(16);
        sealed(|| {
            create_write_sync(&mut sim, &mut out, 1, "/x", 0xAA);
            sync_dir_path(&mut sim, &mut out, 10, "/");

            unlink_path(&mut sim, &mut out, 20, "/x");
            fail_dir_sync(&mut sim, &mut out, 30, "/");

            create_write_sync(&mut sim, &mut out, 40, "/x", 0xBB);
            if overflow_before_sync {
                create_write_sync(&mut sim, &mut out, 50, "/p0", 0x11);
                for i in 0..80u64 {
                    let (a, b) = if i % 2 == 0 {
                        ("/p0", "/p1")
                    } else {
                        ("/p1", "/p0")
                    };
                    rename_path(&mut sim, &mut out, 100 + i, a, b);
                }
            }
            sync_dir_path(&mut sim, &mut out, 200, "/");
            if overflow_after_sync {
                create_write_sync(&mut sim, &mut out, 210, "/p0", 0x11);
                for i in 0..80u64 {
                    let (a, b) = if i % 2 == 0 {
                        ("/p0", "/p1")
                    } else {
                        ("/p1", "/p0")
                    };
                    rename_path(&mut sim, &mut out, 300 + i, a, b);
                }
            }
            sim.power_loss();
            assert_file_byte(&mut sim, &mut out, 400, "/x", 0xBB);
        });
    }
}

#[test]
fn poisoned_rename_then_recreate_file_and_sync_survives_power_loss() {
    let mut sim = make_sim(67, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        create_write_sync(&mut sim, &mut out, 1, "/x", 0xAA);
        sync_dir_path(&mut sim, &mut out, 10, "/");

        rename_path(&mut sim, &mut out, 20, "/x", "/y");
        fail_dir_sync(&mut sim, &mut out, 30, "/");

        create_write_sync(&mut sim, &mut out, 40, "/x", 0xBB);
        sync_dir_path(&mut sim, &mut out, 50, "/");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 60, "/x", 0xBB);
        assert_eq!(
            open_path(&mut sim, &mut out, 70, FileSlot(1), "/y", rw_open()),
            -ENOENT
        );
    });
}

#[test]
fn poisoned_unlink_or_nonempty_dir_rename_then_mkdir_or_replace_survives() {
    // 1. Poisoned Unlink("/x") -> Mkdir("/x") -> sync -> power_loss preserves dir "/x".
    let mut sim = make_sim(71, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        create_write_sync(&mut sim, &mut out, 1, "/x", 0xAA);
        sync_dir_path(&mut sim, &mut out, 10, "/");

        unlink_path(&mut sim, &mut out, 20, "/x");
        fail_dir_sync(&mut sim, &mut out, 30, "/");

        mkdir_path(&mut sim, &mut out, 40, "/x");
        sync_dir_path(&mut sim, &mut out, 50, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 60, FileSlot(0), "/x", dir_open()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 61, Op::FileClose { slot: FileSlot(0) }),
            0
        );
    });

    // 2. Poisoned Rename("/d", "/z") of non-empty dir -> recreate file "/d" -> sync -> power_loss.
    sim = make_sim(73, 512, 16, 16 * 1024);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        create_write_sync(&mut sim, &mut out, 20, "/d/f", 0xCC);
        sync_dir_path(&mut sim, &mut out, 30, "/d");

        rename_path(&mut sim, &mut out, 40, "/d", "/z");
        fail_dir_sync(&mut sim, &mut out, 50, "/");

        create_write_sync(&mut sim, &mut out, 60, "/d", 0xDD);
        sync_dir_path(&mut sim, &mut out, 70, "/");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 80, "/d", 0xDD);
        assert_eq!(
            open_path(&mut sim, &mut out, 90, FileSlot(1), "/d/f", rw_open()),
            -ENOENT
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 91, FileSlot(1), "/d/f", rw_create()),
            -ENOTDIR
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 92, FileSlot(1), "/z", dir_open()),
            -ENOENT
        );
    });

    // 3. Poisoned Rename("/d", "/z") of non-empty dir -> Rename("/z/sub", "/d") -> sync -> power_loss.
    sim = make_sim(79, 512, 16, 16 * 1024);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/d/sub");
        create_write_sync(&mut sim, &mut out, 21, "/d/other", 0x11);
        sync_dir_path(&mut sim, &mut out, 30, "/d");
        create_write_sync(&mut sim, &mut out, 40, "/d/sub/f", 0xEE);
        sync_dir_path(&mut sim, &mut out, 50, "/d/sub");

        rename_path(&mut sim, &mut out, 60, "/d", "/z");
        fail_dir_sync(&mut sim, &mut out, 70, "/");

        rename_path(&mut sim, &mut out, 80, "/z/sub", "/d");
        sync_dir_path(&mut sim, &mut out, 90, "/");
        sync_dir_path(&mut sim, &mut out, 95, "/z");
        sim.power_loss();

        assert_file_byte(&mut sim, &mut out, 100, "/d/f", 0xEE);
        assert_eq!(
            open_path(&mut sim, &mut out, 110, FileSlot(1), "/d/other", rw_open()),
            -ENOENT
        );
    });
}

#[test]
fn dir_log_overflow_and_sync_dir_reclaim_overwritten_durable_inodes() {
    // Variant A: dir_log overflow drops poisoned Unlink("/a") and then Rename("/b", "/a")
    // overwrites durable "/a", which must free "/a"'s inode and data_used slot immediately.
    let cfg_a = DiskConfig {
        max_files: 2,
        max_dirs: 1,
        max_bytes_per_file: 4096,
        max_name_len: 64,
        write_log_entries: 8,
        write_log_bytes: 4096,
        read_error_sectors: 4,
        sector_size: 512,
    };
    let mut sim = SimIo::new(base_limits(), 83, Faults::default());
    sim.setup_disk(cfg_a);
    let mut out = Completions::new(16);
    sealed(|| {
        create_write_sync(&mut sim, &mut out, 1, "/a", 0xAA);
        sync_dir_path(&mut sim, &mut out, 10, "/");

        unlink_path(&mut sim, &mut out, 20, "/a");
        fail_dir_sync(&mut sim, &mut out, 30, "/");

        create_write_sync(&mut sim, &mut out, 40, "/b", 0xBB);
        sync_dir_path(&mut sim, &mut out, 50, "/");

        for i in 0..80u64 {
            let (old_p, new_p) = if i % 2 == 0 {
                ("/b", "/a")
            } else {
                ("/a", "/b")
            };
            rename_path(&mut sim, &mut out, 100 + i, old_p, new_p);
        }

        assert_eq!(
            open_path(&mut sim, &mut out, 200, FileSlot(0), "/c", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 201, Op::FileClose { slot: FileSlot(0) }),
            0
        );
    });

    // Variant B: sync_dir folds Rename("/d1/t", "/d1/a") after overflow dropped poisoned Unlink.
    let cfg_b = DiskConfig {
        max_files: 2,
        max_dirs: 3,
        max_bytes_per_file: 4096,
        max_name_len: 64,
        write_log_entries: 8,
        write_log_bytes: 4096,
        read_error_sectors: 4,
        sector_size: 512,
    };
    sim = SimIo::new(base_limits(), 89, Faults::default());
    sim.setup_disk(cfg_b);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        mkdir_path(&mut sim, &mut out, 2, "/d2");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 15, "/d1/t");
        create_write_sync(&mut sim, &mut out, 20, "/d1/a", 0xAA);
        sync_dir_path(&mut sim, &mut out, 30, "/d1");

        unlink_path(&mut sim, &mut out, 40, "/d1/a");
        fail_dir_sync(&mut sim, &mut out, 50, "/d1");

        create_write_sync(&mut sim, &mut out, 60, "/d2/b", 0xBB);
        sync_dir_path(&mut sim, &mut out, 70, "/d2");

        // 64 renames on a directory (/d1/t <-> /d1/a) are all blocked by poisoned Unlink("/d1/a"),
        // so the 64th rename drops poisoned dir_log[0] without evicting Rename("/d1/t", "/d1/a").
        for i in 0..64u64 {
            let (old_p, new_p) = if i % 2 == 0 {
                ("/d1/t", "/d1/a")
            } else {
                ("/d1/a", "/d1/t")
            };
            rename_path(&mut sim, &mut out, 100 + i, old_p, new_p);
        }

        // Now sync_dir("/d1") folds Rename("/d1/t", "/d1/a"), overwriting durable "/d1/a" (a1).
        sync_dir_path(&mut sim, &mut out, 210, "/d1");

        assert_eq!(
            open_path(&mut sim, &mut out, 230, FileSlot(0), "/d2/c", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 231, Op::FileClose { slot: FileSlot(0) }),
            0
        );
    });

    // Variant C: dir_log overflow drops poisoned Unlink("/a") and evicts Create("/a", a2),
    // reclaiming overwritten durable a1 immediately under max_files: 2.
    let cfg_c = DiskConfig {
        max_files: 2,
        max_dirs: 1,
        max_bytes_per_file: 4096,
        max_name_len: 64,
        write_log_entries: 8,
        write_log_bytes: 4096,
        read_error_sectors: 4,
        sector_size: 512,
    };
    sim = SimIo::new(base_limits(), 97, Faults::default());
    sim.setup_disk(cfg_c);
    sealed(|| {
        create_write_sync(&mut sim, &mut out, 1, "/a", 0xAA);
        sync_dir_path(&mut sim, &mut out, 10, "/");

        unlink_path(&mut sim, &mut out, 20, "/a");
        fail_dir_sync(&mut sim, &mut out, 30, "/");

        create_write_sync(&mut sim, &mut out, 40, "/a", 0xBB);
        for i in 0..70u64 {
            let (old_p, new_p) = if i % 2 == 0 {
                ("/a", "/p0")
            } else {
                ("/p0", "/a")
            };
            rename_path(&mut sim, &mut out, 100 + i, old_p, new_p);
        }

        assert_eq!(
            open_path(&mut sim, &mut out, 200, FileSlot(0), "/c", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 201, Op::FileClose { slot: FileSlot(0) }),
            0
        );
    });
}
