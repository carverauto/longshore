// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-09`–`T3-10`: `read_error` + `refused_punch`
//! interaction, and open inode survival across unsynced rename + `power_loss`.

use super::super::harness::{read_pool_bytes, submit_and_wait_one, write_pool_bytes};
use super::{disk_config, file_limits, open_file, write_path};
use longshore::io::{BufId, Completions, FileSlot, Op, OpenFlags, Token, ENOENT};
use longshore::sim::{DiskFaults, Faults, SimIo};

const EIO: i32 = 5;
const EOPNOTSUPP: i32 = 95;

#[test]
fn t3_09_read_error_and_refused_punch_interaction() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 309, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(1),
            FileSlot(0),
            BufId(4),
            "/t3_09.dat",
            OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x55; 8192]);
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 8192,
                file_off: 0
            },
            1000
        )
        .unwrap()
        .result,
        8192
    );
    sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..Default::default()
    });
    let err_r = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(3),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(1),
            off: 0,
            len: 4096,
            file_off: 4096,
        },
        1000,
    )
    .unwrap();
    assert_eq!(err_r.result, -EIO);
    sim.set_disk_faults(DiskFaults::default());

    sim.set_disk_faults(DiskFaults {
        punch_refused: 1_000_000,
        ..Default::default()
    });
    let p_err = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(4),
        Op::FilePunch {
            slot: FileSlot(0),
            file_off: 4096,
            len: 4096,
        },
        1000,
    )
    .unwrap();
    assert_eq!(p_err.result, -EOPNOTSUPP);
    sim.set_disk_faults(DiskFaults::default());

    let still_err = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(5),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(1),
            off: 0,
            len: 4096,
            file_off: 4096,
        },
        1000,
    )
    .unwrap();
    assert_eq!(still_err.result, -EIO);

    let p_ok = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(6),
        Op::FilePunch {
            slot: FileSlot(0),
            file_off: 4096,
            len: 4096,
        },
        1000,
    )
    .unwrap();
    assert_eq!(p_ok.result, 0);

    let after_r = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(7),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(1),
            off: 0,
            len: 4096,
            file_off: 4096,
        },
        1000,
    )
    .unwrap();
    assert_eq!(after_r.result, 4096);
    assert!(read_pool_bytes(&mut sim, BufId(1), 0, 4096)
        .iter()
        .all(|&b| b == 0));
}

#[test]
fn t3_10_open_inode_survives_unsynced_rename_and_power_loss() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 310, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(1),
            FileSlot(0),
            BufId(4),
            "/a.dat",
            OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(2),
            FileSlot(1),
            BufId(4),
            "/",
            OpenFlags {
                read: true,
                directory: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(3),
            Op::FileSync {
                slot: FileSlot(1),
                datasync: false
            },
            1000
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(4),
            Op::FileClose { slot: FileSlot(1) },
            1000
        )
        .unwrap()
        .result,
        0
    );

    let old_len = write_path(&mut sim, BufId(4), "/a.dat");
    let new_len = write_path(&mut sim, BufId(5), "/b.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(5),
            Op::FileRename {
                old_buf: BufId(4),
                old_off: 0,
                old_len,
                new_buf: BufId(5),
                new_off: 0,
                new_len,
            },
            1000
        )
        .unwrap()
        .result,
        0
    );

    let expected = [0x9A; 512];
    write_pool_bytes(&mut sim, BufId(0), 0, &expected);
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(6),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 512,
                file_off: 0
            },
            1000
        )
        .unwrap()
        .result,
        512
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(7),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true
            },
            1000
        )
        .unwrap()
        .result,
        0
    );

    sim.power_loss();
    let a_ok = open_file(
        &mut sim,
        &mut out,
        Token(8),
        FileSlot(0),
        BufId(4),
        "/a.dat",
        OpenFlags {
            read: true,
            ..Default::default()
        },
    )
    .unwrap();
    let b_ok = open_file(
        &mut sim,
        &mut out,
        Token(9),
        FileSlot(1),
        BufId(4),
        "/b.dat",
        OpenFlags {
            read: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_ne!(
        a_ok == 0,
        b_ok == 0,
        "exactly one of /a.dat or /b.dat must exist: a={a_ok} b={b_ok}"
    );
    let slot = if a_ok == 0 { FileSlot(0) } else { FileSlot(1) };
    let r = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(10),
        Op::FileRead {
            slot,
            buf: BufId(1),
            off: 0,
            len: 512,
            file_off: 0,
        },
        1000,
    )
    .unwrap();
    assert_eq!(r.result, 512);
    assert_eq!(read_pool_bytes(&mut sim, BufId(1), 0, 512), expected);
    if a_ok != 0 {
        assert_eq!(a_ok, -ENOENT);
    }
}
