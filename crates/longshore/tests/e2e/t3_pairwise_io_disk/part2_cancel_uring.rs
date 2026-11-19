// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-03`–`T3-05`: chain cancellation,
//! `IORING_SETUP_SUBMIT_ALL` + ready list, and direct-I/O whole-chain refusal.

#[cfg(target_os = "linux")]
use super::super::harness::TempDir;
use super::super::harness::{drive_until, submit_and_wait_one, write_pool_bytes};
use super::{disk_config, file_limits, open_file, write_path};
use longshore::io::{
    BufId, Completions, FileSlot, Io, Op, OpenFlags, Token, ECANCELED, EINVAL, ENOENT,
};
use longshore::sim::{Faults, SimIo};

#[test]
fn t3_03_cancel_running_head_vs_trailing_chain_member() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(
        limits,
        303,
        Faults {
            delay: 500_000,
            ..Default::default()
        },
    );
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    let plen = write_path(&mut sim, BufId(4), "/t3_03.dat");
    write_pool_bytes(&mut sim, BufId(0), 0, &[0xAB; 4096]);
    let chain = [
        (
            Token(100),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(4),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    ..Default::default()
                },
            },
        ),
        (
            Token(101),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        (
            Token(102),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ];
    sim.submit_chain(&chain).unwrap();
    sim.submit(Token(200), Op::Cancel { target: Token(102) })
        .unwrap();
    let cs = drive_until(&mut sim, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(200)) && v.iter().any(|c| c.token == Token(102))
    })
    .unwrap();
    assert!(cs.iter().find(|c| c.token == Token(200)).unwrap().result < 0);
    assert_eq!(cs.iter().find(|c| c.token == Token(102)).unwrap().result, 0);

    let chain2 = [
        (
            Token(110),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 4096,
                file_off: 4096,
            },
        ),
        (
            Token(111),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ];
    sim.submit_chain(&chain2).unwrap();
    sim.submit(Token(201), Op::Cancel { target: Token(110) })
        .unwrap();
    let cs2 = drive_until(&mut sim, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(110))
            && v.iter().any(|c| c.token == Token(111))
            && v.iter().any(|c| c.token == Token(201))
    })
    .unwrap();
    let head = cs2.iter().find(|c| c.token == Token(110)).unwrap().result;
    let tail = cs2.iter().find(|c| c.token == Token(111)).unwrap().result;
    if head < 0 {
        assert_eq!(head, -ECANCELED);
        assert_eq!(tail, -ECANCELED);
    }
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(120),
            Op::FileClose { slot: FileSlot(0) },
            10_000,
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(121),
            FileSlot(0),
            BufId(4),
            "/t3_03.dat",
            OpenFlags {
                read: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
}

#[cfg(target_os = "linux")]
#[test]
fn t3_04_uring_submit_all_ready_list_and_valid_chain() {
    let dir = TempDir::new("t3_04").unwrap();
    let file_p = dir.file_path("ok.dat");
    let limits = file_limits(4, 2, 4096);
    let mut io = longshore::io::uring::UringIo::new(limits).unwrap();
    let mut out = Completions::new(16);

    assert_eq!(
        open_file(
            &mut io,
            &mut out,
            Token(1),
            FileSlot(0),
            BufId(4),
            file_p.to_str().unwrap(),
            OpenFlags {
                read: true,
                write: true,
                create: true,
                direct: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );

    let missing_len = write_path(&mut io, BufId(5), dir.file_path("nope").to_str().unwrap());
    write_pool_bytes(&mut io, BufId(8), 0, &[0x5A; 4096]);
    io.submit(
        Token(10),
        Op::FileStatus {
            path_buf: BufId(5),
            path_off: 0,
            path_len: missing_len,
            out_buf: BufId(6),
            out_off: 0,
        },
    )
    .unwrap();
    io.submit_chain(&[
        (
            Token(11),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(8),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        (
            Token(12),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    io.submit(
        Token(13),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: BufId(0),
            off: 0,
            len: 17,
            file_off: 0,
        },
    )
    .unwrap();

    let cs = drive_until(&mut io, &mut out, 10_000, |v| {
        [10, 11, 12, 13]
            .iter()
            .all(|t| v.iter().any(|c| c.token == Token(*t)))
    })
    .unwrap();
    assert_eq!(
        cs.iter().find(|c| c.token == Token(10)).unwrap().result,
        -ENOENT
    );
    assert_eq!(
        cs.iter().find(|c| c.token == Token(11)).unwrap().result,
        4096
    );
    assert_eq!(cs.iter().find(|c| c.token == Token(12)).unwrap().result, 0);
    assert_eq!(
        cs.iter().find(|c| c.token == Token(13)).unwrap().result,
        -EINVAL
    );
}

#[test]
fn t3_05_direct_open_and_unaligned_write_chain_whole_refusal() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 305, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    let plen = write_path(&mut sim, BufId(4), "/t3_05_never_created.dat");
    write_pool_bytes(&mut sim, BufId(0), 0, &[0xCC; 128]);
    let chain = [
        (
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(4),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    direct: true,
                    ..Default::default()
                },
            },
        ),
        (
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 100,
                file_off: 0,
            },
        ),
        (
            Token(3),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ];
    sim.submit_chain(&chain).unwrap();
    let cs = drive_until(&mut sim, &mut out, 100, |v| v.len() == 3).unwrap();
    assert_eq!(
        cs.iter().find(|c| c.token == Token(1)).unwrap().result,
        -ECANCELED
    );
    assert_eq!(
        cs.iter().find(|c| c.token == Token(2)).unwrap().result,
        -EINVAL
    );
    assert_eq!(
        cs.iter().find(|c| c.token == Token(3)).unwrap().result,
        -ECANCELED
    );

    let st = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(4),
        Op::FileStatus {
            path_buf: BufId(4),
            path_off: 0,
            path_len: plen,
            out_buf: BufId(5),
            out_off: 0,
        },
        100,
    )
    .unwrap();
    assert_eq!(st.result, -ENOENT);
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(5),
            FileSlot(0),
            BufId(4),
            "/t3_05_after.dat",
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
}
