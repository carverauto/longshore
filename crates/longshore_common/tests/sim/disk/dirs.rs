// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Directory sync, cross-directory rename, and open-inode survival tests.

use super::{
    dir_open, drive, make_sim, open_path, put_path, read_at, rw_create, rw_open, sync_dir_path,
    sync_slot, write_fill,
};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::{EIO, ENOENT};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskFaults, SimIo};

#[path = "dirs_overflow.rs"]
mod overflow;
#[path = "dirs_reclaim.rs"]
mod reclaim;
#[path = "dirs_sync_fail.rs"]
mod sync_fail;

#[test]
fn open_file_survives_unlink_and_rename_and_cross_dir_rename_sync() {
    let mut sim = make_sim(5, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    let d1 = put_path(&mut sim, BufId(0), "/d1");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            1,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: d1,
            },
        ),
        0
    );
    let d2 = put_path(&mut sim, BufId(0), "/d2");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            2,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: d2,
            },
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");

    assert_eq!(
        open_path(&mut sim, &mut out, 20, FileSlot(0), "/d1/f", rw_create()),
        0
    );
    sim.buf(BufId(1))[..5].copy_from_slice(b"alive");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            21,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 5,
                file_off: 0,
            },
        ),
        5
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            22,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 30, "/d1");

    let ol = put_path(&mut sim, BufId(0), "/d1/f");
    let nl = put_path(&mut sim, BufId(1), "/d2/g");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            40,
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len: ol,
                new_buf: BufId(1),
                new_off: 0,
                new_len: nl,
            },
        ),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            41,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 5,
                file_off: 0,
            },
        ),
        5
    );
    assert_eq!(&sim.buf(BufId(2))[..5], b"alive");

    let ul = put_path(&mut sim, BufId(0), "/d2/g");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            42,
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        0
    );
    let sl = put_path(&mut sim, BufId(0), "/d2/g");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            43,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: sl,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        -ENOENT
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            44,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 5,
                file_off: 0,
            },
        ),
        5
    );
    assert_eq!(&sim.buf(BufId(2))[..5], b"alive");
    assert_eq!(
        drive(&mut sim, &mut out, 45, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    sync_dir_path(&mut sim, &mut out, 50, "/d1");
    sync_dir_path(&mut sim, &mut out, 60, "/d2");
    sim.power_loss();
    let sl2 = put_path(&mut sim, BufId(0), "/d1/f");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            70,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: sl2,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        -ENOENT
    );
}

pub(super) fn fail_dir_sync(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(open_path(sim, out, tok, FileSlot(3), path, dir_open()), 0);
    sim.set_disk_faults(DiskFaults {
        sync_fail: 1_000_000,
        ..Default::default()
    });
    assert_eq!(sync_slot(sim, out, tok + 1, FileSlot(3)), -EIO);
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(sim, out, tok + 2, Op::FileClose { slot: FileSlot(3) }),
        0
    );
}

pub(super) fn rename_path(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    old_p: &str,
    new_p: &str,
) {
    let ol = put_path(sim, BufId(0), old_p);
    let nl = put_path(sim, BufId(1), new_p);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len: ol,
                new_buf: BufId(1),
                new_off: 0,
                new_len: nl,
            },
        ),
        0
    );
}

pub(super) fn unlink_path(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    let ul = put_path(sim, BufId(0), path);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        0
    );
}

pub(super) fn mkdir_path(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    let len = put_path(sim, BufId(0), path);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: len,
            },
        ),
        0
    );
}

#[test]
fn dir_sync_fail_poisoning_sibling_does_not_block_later_synced_sibling() {
    let mut sim = make_sim(17, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot(0), "/f1", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 2, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 10, "/");

        assert_eq!(
            open_path(&mut sim, &mut out, 20, FileSlot(0), "/f2", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 21, FileSlot(0), 0, 5, 0x42),
            5
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 22, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 23, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 30, "/");

        fail_dir_sync(&mut sim, &mut out, 40, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 50, FileSlot(0), "/f1", rw_open()),
            -ENOENT
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 51, FileSlot(0), "/f2", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 52, FileSlot(0), 0, 5), 5);
        assert!(sim.buf(BufId(2))[..5].iter().all(|&b| b == 0x42));
    });
}
