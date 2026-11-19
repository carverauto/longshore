// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Directory sync, cross-directory rename, and open-inode survival tests.

use super::{drive, make_sim, open_path, put_path, rw_create, sync_dir_path};
use longshore_common::io::posix::errno::linux::ENOENT;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};

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
