// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` mkdir, parent-sync, medium corruption, and path-rule tests.

use super::{
    drive, make_storage, mkdir, open_file, put_path, read_all, rw_create, sync_dir, write_bytes,
};
use longshore_common::io::posix::errno::linux::{EISDIR, ENOENT, ENOTDIR};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, EINVAL};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[test]
fn mkdir_not_synced_into_parent_can_be_lost() {
    let mut saw_lost = false;
    for seed in 0..16u64 {
        let limits = Limits {
            buffers: 2,
            buffer_size: 4096,
            files_max: 2,
            ops_max: 8,
            ..Limits::BASE
        };
        let mut sim = SimIo::new(limits, seed, Faults::default());
        sim.setup_disk(DiskConfig {
            max_files: 4,
            max_dirs: 4,
            max_bytes_per_file: 4096,
            max_name_len: 64,
            write_log_entries: 8,
            write_log_bytes: 4096,
            read_error_sectors: 4,
            sector_size: 512,
        });
        let mut out = Completions::new(8);
        assert_eq!(mkdir(&mut sim, &mut out, 1, "/x"), 0);
        sim.power_loss();
        let len = put_path(&mut sim, BufId(0), "/x");
        let r = drive(
            &mut sim,
            &mut out,
            2,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: len,
                out_buf: BufId(1),
                out_off: 0,
            },
        );
        if r == -ENOENT {
            saw_lost = true;
        }
    }
    assert!(saw_lost);
}

#[test]
fn seeded_file_and_parents_synced_survive_power_loss() {
    let (mut sim, mut out, _) = make_storage(1, 512);
    assert_eq!(mkdir(&mut sim, &mut out, 10, "/d/a"), 0);
    assert_eq!(mkdir(&mut sim, &mut out, 11, "/d/a/b"), 0);
    assert_eq!(
        open_file(&mut sim, &mut out, 12, FileSlot(0), "/d/a/b/c", rw_create()),
        0
    );
    assert_eq!(
        write_bytes(&mut sim, &mut out, 13, FileSlot(0), 0, b"seeded"),
        6
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            14,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );
    assert_eq!(
        drive(&mut sim, &mut out, 15, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    sync_dir(&mut sim, &mut out, 20, "/d");
    sync_dir(&mut sim, &mut out, 25, "/d/a");
    sync_dir(&mut sim, &mut out, 30, "/d/a/b");
    sim.power_loss();
    assert_eq!(
        read_all(&mut sim, &mut out, 40, "/d/a/b/c"),
        Ok(b"seeded".to_vec())
    );
}

#[test]
fn corrupt_medium_bit_flips_both_views_and_rejects_oob_and_missing() {
    let (mut sim, mut out, _) = make_storage(1, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, 10, FileSlot(0), "/d/f", rw_create()),
        0
    );
    assert_eq!(
        write_bytes(&mut sim, &mut out, 11, FileSlot(0), 0, &[0x0F, 0x0F]),
        2
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            12,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );
    assert_eq!(
        write_bytes(&mut sim, &mut out, 13, FileSlot(0), 0, &[0x0F, 0x0F]),
        2
    );
    assert_eq!(
        drive(&mut sim, &mut out, 14, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    sync_dir(&mut sim, &mut out, 15, "/d");

    for bit in 0..8u8 {
        sim.corrupt_medium_bit("/d/f", 1, bit).unwrap();
    }
    assert_eq!(
        read_all(&mut sim, &mut out, 20, "/d/f"),
        Ok(vec![0x0F, 0xF0])
    );
    sim.power_loss();
    assert_eq!(
        read_all(&mut sim, &mut out, 50, "/d/f"),
        Ok(vec![0x0F, 0xF0])
    );

    assert_eq!(sim.corrupt_medium_bit("/d/f", 2, 0), Err(-EINVAL));
    assert_eq!(sim.corrupt_medium_bit("/d/g", 0, 0), Err(-ENOENT));
}

#[test]
fn invalid_paths_and_dir_vs_file_errors() {
    let (mut sim, mut out, _) = make_storage(1, 512);
    sim.buf(BufId(0))[0] = 0;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            10,
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: 1,
                flags: rw_create(),
            },
        ),
        -EINVAL
    );
    assert_eq!(
        open_file(&mut sim, &mut out, 11, FileSlot(0), "/d/f", rw_create()),
        0
    );
    assert_eq!(
        drive(&mut sim, &mut out, 12, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(
        open_file(&mut sim, &mut out, 13, FileSlot(0), "/d/f/g", rw_create()),
        -ENOTDIR
    );
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            14,
            FileSlot(0),
            "/",
            OpenFlags {
                read: true,
                ..Default::default()
            },
        ),
        0
    );
    assert_eq!(
        drive(&mut sim, &mut out, 15, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(
        open_file(&mut sim, &mut out, 16, FileSlot(0), "/", rw_create()),
        -EISDIR
    );
}

#[test]
fn directory_is_not_a_file_across_open_corrupt_unlink_rename() {
    let (mut sim, mut out, _) = make_storage(1, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, 10, FileSlot(0), "/d", rw_create()),
        -EISDIR
    );
    assert_eq!(sim.corrupt_medium_bit("/d", 0, 0), Err(-EISDIR));
    let ul = put_path(&mut sim, BufId(0), "/d");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            11,
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        -EISDIR
    );
    assert_eq!(
        open_file(&mut sim, &mut out, 12, FileSlot(0), "/d/f", rw_create()),
        0
    );
    assert_eq!(
        drive(&mut sim, &mut out, 13, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(mkdir(&mut sim, &mut out, 14, "/d/sub"), 0);
    let ol = put_path(&mut sim, BufId(0), "/d/f");
    let nl = put_path(&mut sim, BufId(1), "/d/sub");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            15,
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len: ol,
                new_buf: BufId(1),
                new_off: 0,
                new_len: nl,
            },
        ),
        -EISDIR
    );
}
