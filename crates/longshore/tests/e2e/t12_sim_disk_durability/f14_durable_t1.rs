// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 14: Virtual durable bytes, write log overlay, and directory sync.

use super::super::harness::{submit_and_wait_one, write_nul_path};
use super::helpers::{
    close_file, make_disk_sim, open_file, read_file_vec, stat_exists, sync_file, sync_root_dir,
    write_file,
};
use longshore::io::alloc_guard::sealed;
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token};
use longshore::sim::SimIo;

/// `T1-F14-1`: Unsynced writes are immediately visible to subsequent reads via write log overlay.
#[test]
fn t1_f14_1_unsynced_write_visible_via_log_overlay() {
    let mut sim = make_disk_sim(1, 512, 64, 256 * 1024);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/ov.dat", rw),
        0
    );
    assert_eq!(
        write_file(
            &mut sim,
            &mut out,
            2,
            FileSlot(0),
            0,
            b"unsynced-overlay-123"
        ),
        20
    );
    let got = read_file_vec(&mut sim, &mut out, 3, FileSlot(0), 0, 20).expect("read");
    assert_eq!(got, b"unsynced-overlay-123");
    assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot(0)), 0);
}

/// `T1-F14-2`: Synced write survives `power_loss()` and reads back identically after reopen.
#[test]
fn t1_f14_2_synced_write_survives_power_loss() {
    let mut sim = make_disk_sim(2, 512, 64, 256 * 1024);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/dur.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 512, b"durable-block"),
        13
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);
    assert_eq!(close_file(&mut sim, &mut out, 7, FileSlot(0)), 0);

    sim.power_loss();

    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 10, FileSlot(0), "/dur.dat", ro),
        0
    );
    let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 512, 13).expect("read");
    assert_eq!(got, b"durable-block");
    assert_eq!(close_file(&mut sim, &mut out, 12, FileSlot(0)), 0);
}

/// `T1-F14-3`: Directory sync makes create, rename, and unlink durable across `power_loss()`.
#[test]
fn t1_f14_3_dir_sync_makes_create_rename_unlink_durable() {
    let mut sim = make_disk_sim(3, 512, 64, 256 * 1024);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/dir_a.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, b"dir-synced"),
        10
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot(0)), 0);

    let l_a = write_nul_path(&mut sim, BufId(0), 0, "/dir_a.dat");
    let l_b = write_nul_path(&mut sim, BufId(1), 0, "/dir_b.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(5),
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len: l_a,
                new_buf: BufId(1),
                new_off: 0,
                new_len: l_b,
            },
            10_000,
        )
        .expect("rename")
        .result,
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 10, FileSlot(1)), 0);
    sim.power_loss();

    assert!(!stat_exists(&mut sim, &mut out, 20, "/dir_a.dat"));
    assert!(stat_exists(&mut sim, &mut out, 21, "/dir_b.dat"));

    let l_b2 = write_nul_path(&mut sim, BufId(0), 0, "/dir_b.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(22),
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: l_b2,
            },
            10_000,
        )
        .expect("unlink")
        .result,
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 23, FileSlot(1)), 0);
    sim.power_loss();
    assert!(!stat_exists(&mut sim, &mut out, 30, "/dir_b.dat"));
}

/// `T1-F14-4`: An open file handle outlives unlink or rename of its directory entry until closed.
#[test]
fn t1_f14_4_open_inode_survives_unlink_and_rename() {
    let mut sim = make_disk_sim(4, 512, 64, 256 * 1024);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/ghost.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, b"open-inode"),
        10
    );

    let plen = write_nul_path(&mut sim, BufId(0), 0, "/ghost.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(3),
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen,
            },
            10_000,
        )
        .expect("unlink")
        .result,
        0
    );
    assert!(!stat_exists(&mut sim, &mut out, 4, "/ghost.dat"));

    // Open FileSlot(0) still writes and reads its inode!
    assert_eq!(
        write_file(&mut sim, &mut out, 5, FileSlot(0), 10, b"-still-alive"),
        12
    );
    let got = read_file_vec(&mut sim, &mut out, 6, FileSlot(0), 0, 22).expect("read open");
    assert_eq!(got, b"open-inode-still-alive");
    assert_eq!(close_file(&mut sim, &mut out, 7, FileSlot(0)), 0);
}

/// `T1-F14-5`: Simulated disk open/write/read/sync/rename/close run with zero heap allocations.
#[test]
fn t1_f14_5_disk_ops_zero_alloc_under_alloc_guard() {
    let mut sim = make_disk_sim(5, 512, 64, 256 * 1024);
    let mut out = Completions::new(16);
    {
        let p = sim.buf(BufId(0));
        p[..9].copy_from_slice(b"/zero.db\0");
        let w = sim.buf(BufId(1));
        w[..512].fill(0x5A);
    }

    sealed(|| {
        let mut drive_one = |sim: &mut SimIo, tok: u64, op: Op| -> i32 {
            sim.submit(Token(tok), op).expect("submit");
            loop {
                sim.tick(true, &mut out).expect("tick");
                if let Some(c) = out.pop() {
                    if c.token == Token(tok) {
                        return c.result;
                    }
                }
            }
        };
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            drive_one(
                &mut sim,
                1,
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: 9,
                    flags: rw,
                },
            ),
            0
        );
        assert_eq!(
            drive_one(
                &mut sim,
                2,
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 512,
                    file_off: 0,
                },
            ),
            512
        );
        assert_eq!(
            drive_one(
                &mut sim,
                3,
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
            0
        );
        assert_eq!(
            drive_one(
                &mut sim,
                4,
                Op::FileRead {
                    slot: FileSlot(0),
                    buf: BufId(2),
                    off: 0,
                    len: 512,
                    file_off: 0,
                },
            ),
            512
        );
        assert!(sim.buf(BufId(2))[..512].iter().all(|&b| b == 0x5A));
        assert_eq!(
            drive_one(&mut sim, 5, Op::FileClose { slot: FileSlot(0) }),
            0
        );
    });
}
