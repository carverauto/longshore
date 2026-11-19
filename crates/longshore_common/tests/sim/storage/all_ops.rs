// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` test proving placed faults reach all 11 file operations.

use super::{drive, make_storage, open_file, put_path, rw_create, sync_dir, write_bytes};
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{BufId, FileSlot, Op};
use longshore_common::sim::PlacedFault;

#[test]
fn placed_fail_reaches_all_eleven_file_ops() {
    let (mut sim, mut out, mut tok) = make_storage(1, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, tok, FileSlot(0), "/d/f", rw_create()),
        0
    );
    tok += 1;
    sync_dir(&mut sim, &mut out, tok, "/d");
    tok += 5;
    let mut next_op = 9u64;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        open_file(&mut sim, &mut out, tok, FileSlot(1), "/d/n", rw_create()),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        open_file(&mut sim, &mut out, tok, FileSlot(1), "/d/n", rw_create()),
        0
    );
    tok += 1;
    next_op += 1;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot(0), 0, b"abc"),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot(0), 0, b"abc"),
        3
    );
    tok += 1;
    next_op += 1;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 3,
                file_off: 0,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 3,
                file_off: 0,
            },
        ),
        3
    );
    tok += 1;
    next_op += 1;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );
    tok += 1;
    next_op += 1;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
        ),
        0
    );
    tok += 1;
    next_op += 1;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
            },
        ),
        0
    );
    tok += 1;
    next_op += 1;

    let sl = put_path(&mut sim, BufId(0), "/d/f");
    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: sl,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: sl,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        0
    );
    tok += 1;
    next_op += 1;

    let dl = put_path(&mut sim, BufId(0), "/d/e");
    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: dl,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: dl,
            },
        ),
        0
    );
    tok += 1;
    next_op += 1;

    let ol = put_path(&mut sim, BufId(0), "/d/n");
    let nl = put_path(&mut sim, BufId(1), "/d/m");
    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
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
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
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
    tok += 1;
    next_op += 1;

    let ul = put_path(&mut sim, BufId(0), "/d/m");
    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        -EIO
    );
    tok += 1;
    next_op += 1;
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        0
    );
    tok += 1;
    next_op += 1;

    sim.place_disk_fault(next_op, PlacedFault::Fail);
    assert_eq!(
        drive(&mut sim, &mut out, tok, Op::FileClose { slot: FileSlot(1) }),
        -EIO
    );
    assert_eq!(sim.disk_fault_counters().placed_fail, 11);
}
