// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 12: Happy-path linked operation chains (`Io::submit_chain`).

use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{
    chain_limits, close_slot, open_slot, run_on_backends, submit_chain_and_collect, DynIo,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token, ECANCELED};

/// `T1-F12-1`: 2-op chain `[FileWrite, FileSync]` executes in order and completes cleanly.
#[test]
fn t1_f12_1_chain_write_then_sync() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let path = format!("{prefix}chain_ws.bin");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, rw),
            0
        );

        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(0), 0, b"chained-write-sync-data");
        let cs = submit_chain_and_collect(
            io,
            &mut out,
            &[
                (
                    Token(10),
                    Op::FileWrite {
                        slot: FileSlot(0),
                        buf: BufId(0),
                        off: 0,
                        len: 23,
                        file_off: 0,
                    },
                ),
                (
                    Token(11),
                    Op::FileSync {
                        slot: FileSlot(0),
                        datasync: true,
                    },
                ),
            ],
        );
        let pos_w = cs.iter().position(|c| c.token == Token(10)).unwrap();
        let pos_s = cs.iter().position(|c| c.token == Token(11)).unwrap();
        assert!(pos_w < pos_s, "write must complete before sync");
        assert_eq!(cs[pos_w].result, 23);
        assert_eq!(cs[pos_s].result, 0);

        let mut d = DynIo(io);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(12),
                Op::FileRead {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 23,
                    file_off: 0,
                },
                10_000,
            )
            .expect("read")
            .result,
            23
        );
        assert_eq!(
            read_pool_bytes(&mut d, BufId(1), 0, 23),
            b"chained-write-sync-data"
        );
        assert_eq!(close_slot(io, &mut out, 13, FileSlot(0)), 0);
    });
}

/// `T1-F12-2`: 4-op chain `[FileOpen, FileWrite, FileSync, FileClose]` frees slot at end.
#[test]
fn t1_f12_2_chain_open_write_sync_close_four_ops() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let path = format!("{prefix}chain_4op.bin");
        let mut d = DynIo(io);
        let path_len = write_nul_path(&mut d, BufId(0), 0, &path);
        write_pool_bytes(&mut d, BufId(1), 0, b"four-stage-chain");

        let cs = submit_chain_and_collect(
            io,
            &mut out,
            &[
                (
                    Token(1),
                    Op::FileOpen {
                        slot: FileSlot(0),
                        path_buf: BufId(0),
                        path_off: 0,
                        path_len,
                        flags: OpenFlags {
                            read: true,
                            write: true,
                            create: true,
                            ..Default::default()
                        },
                    },
                ),
                (
                    Token(2),
                    Op::FileWrite {
                        slot: FileSlot(0),
                        buf: BufId(1),
                        off: 0,
                        len: 16,
                        file_off: 0,
                    },
                ),
                (
                    Token(3),
                    Op::FileSync {
                        slot: FileSlot(0),
                        datasync: false,
                    },
                ),
                (Token(4), Op::FileClose { slot: FileSlot(0) }),
            ],
        );
        let order: Vec<(u64, i32)> = cs.iter().map(|c| (c.token.0, c.result)).collect();
        assert_eq!(order, vec![(1, 0), (2, 16), (3, 0), (4, 0)]);

        // FileSlot(0) must now be Free and reusable.
        let ro = OpenFlags {
            read: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 5, FileSlot(0), BufId(0), &path, ro),
            0
        );
        assert_eq!(close_slot(io, &mut out, 6, FileSlot(0)), 0);
    });
}

/// `T1-F12-3`: `[FileReserve, FileWrite, FileSync]` extends file, writes range, and commits.
#[test]
fn t1_f12_3_chain_reserve_write_sync() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let path = format!("{prefix}chain_rws.bin");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, rw),
            0
        );

        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(0), 0, b"reserved-range");
        let cs = submit_chain_and_collect(
            io,
            &mut out,
            &[
                (
                    Token(10),
                    Op::FileReserve {
                        slot: FileSlot(0),
                        file_off: 0,
                        len: 4096,
                        keep_size: false,
                    },
                ),
                (
                    Token(11),
                    Op::FileWrite {
                        slot: FileSlot(0),
                        buf: BufId(0),
                        off: 0,
                        len: 14,
                        file_off: 1024,
                    },
                ),
                (
                    Token(12),
                    Op::FileSync {
                        slot: FileSlot(0),
                        datasync: true,
                    },
                ),
            ],
        );
        assert_eq!(cs[0].result, 0);
        assert_eq!(cs[1].result, 14);
        assert_eq!(cs[2].result, 0);
        assert_eq!(close_slot(io, &mut out, 13, FileSlot(0)), 0);
    });
}

/// `T1-F12-4`: A failing chain in the same turn as independent ops does not cancel them.
#[test]
fn t1_f12_4_chain_coexists_with_independent_ops() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let missing = format!("{prefix}no_such_chain_file.bin");
        let mut d = DynIo(io);
        d.submit(Token(1), Op::Timeout { nanos: 100_000 }).unwrap();
        let path_len = write_nul_path(&mut d, BufId(0), 0, &missing);
        d.submit_chain(&[
            (
                Token(10),
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len,
                    flags: OpenFlags {
                        read: true,
                        ..Default::default()
                    },
                },
            ),
            (Token(11), Op::FileClose { slot: FileSlot(0) }),
        ])
        .unwrap();
        d.submit(Token(2), Op::Timeout { nanos: 200_000 }).unwrap();

        let cs = drive_until(&mut d, &mut out, 10_000, |v| {
            [1u64, 2, 10, 11]
                .iter()
                .all(|t| v.iter().any(|c| c.token.0 == *t))
        })
        .expect("all 4 complete");
        assert_eq!(cs.iter().find(|c| c.token == Token(1)).unwrap().result, 0);
        assert_eq!(cs.iter().find(|c| c.token == Token(2)).unwrap().result, 0);
        assert!(cs.iter().find(|c| c.token == Token(10)).unwrap().result < 0);
        assert_eq!(
            cs.iter().find(|c| c.token == Token(11)).unwrap().result,
            -ECANCELED
        );
    });
}

/// `T1-F12-5`: Cancel of unstarted trailing chain member returns negative and does not stop chain.
#[test]
fn t1_f12_5_cancel_trailing_chain_member_contract() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let path = format!("{prefix}chain_cancel_tail.bin");
        let mut d = DynIo(io);
        let path_len = write_nul_path(&mut d, BufId(0), 0, &path);
        write_pool_bytes(&mut d, BufId(1), 0, b"tail-cancel-payload");

        d.submit_chain(&[
            (
                Token(10),
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len,
                    flags: OpenFlags {
                        read: true,
                        write: true,
                        create: true,
                        ..Default::default()
                    },
                },
            ),
            (
                Token(11),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 19,
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
        // Cancel the trailing FileSync (which has not started yet).
        d.submit(Token(99), Op::Cancel { target: Token(12) })
            .unwrap();

        let cs = drive_until(&mut d, &mut out, 10_000, |v| {
            [10u64, 11, 12, 99]
                .iter()
                .all(|t| v.iter().any(|c| c.token.0 == *t))
        })
        .expect("all complete");
        assert_eq!(cs.iter().find(|c| c.token == Token(10)).unwrap().result, 0);
        assert_eq!(cs.iter().find(|c| c.token == Token(11)).unwrap().result, 19);
        // Trailing member was not running yet when cancel fired: cancel fails negatively or
        // sync still finishes cleanly; neither hangs nor leaks slot.
        let r_sync = cs.iter().find(|c| c.token == Token(12)).unwrap().result;
        let r_cancel = cs.iter().find(|c| c.token == Token(99)).unwrap().result;
        assert!(
            (r_cancel < 0 && r_sync == 0) || (r_cancel == 0 && r_sync == -ECANCELED),
            "unexpected (cancel={r_cancel}, sync={r_sync})"
        );
        assert_eq!(close_slot(io, &mut out, 200, FileSlot(0)), 0);
    });
}
