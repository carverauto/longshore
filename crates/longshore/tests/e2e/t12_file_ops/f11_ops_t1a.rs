// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 11 (Part A): Open/Write/Read/Close, Sync, and Reserve + `statx`.

use super::super::harness::{
    read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{close_slot, file_ops_limits, open_slot, run_on_backends, stat_size, DynIo};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token};

/// `T1-F11-1`: `FileOpen` + `FileWrite` at offset 4096 + `FileRead` at offset 4096 + `FileClose`.
#[test]
fn t1_f11_1_open_write_read_close_at_offset() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}rw_offset.bin");
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

        let pattern: Vec<u8> = (0..1024u32).map(|i| ((i * 17) ^ 0x3C) as u8).collect();
        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(0), 0, &pattern);
        let wr = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 1024,
                file_off: 4096,
            },
            10_000,
        )
        .expect("write");
        assert_eq!(wr.result, 1024);

        let rd = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(3),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 1024,
                file_off: 4096,
            },
            10_000,
        )
        .expect("read");
        assert_eq!(rd.result, 1024);
        assert_eq!(read_pool_bytes(&mut d, BufId(1), 0, 1024), pattern);
        assert_eq!(close_slot(io, &mut out, 4, FileSlot(0)), 0);
    });
}

/// `T1-F11-2`: `FileSync` (both `datasync=true/false`) on a regular file and a directory.
#[test]
fn t1_f11_2_sync_file_and_directory_datasync() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let p_old = format!("{prefix}sync_old.bin");
        let p_new = format!("{prefix}sync_new.bin");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &p_old, rw),
            0
        );
        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(1), 0, b"sync-bytes");
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 10,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result,
            10
        );
        for (idx, datasync) in [true, false].into_iter().enumerate() {
            assert_eq!(
                submit_and_wait_one(
                    &mut d,
                    &mut out,
                    Token(10 + idx as u64),
                    Op::FileSync {
                        slot: FileSlot(0),
                        datasync,
                    },
                    10_000,
                )
                .expect("sync")
                .result,
                0
            );
        }
        assert_eq!(close_slot(io, &mut out, 20, FileSlot(0)), 0);

        d = DynIo(io);
        let old_len = write_nul_path(&mut d, BufId(0), 0, &p_old);
        let new_len = write_nul_path(&mut d, BufId(1), 0, &p_new);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(21),
                Op::FileRename {
                    old_buf: BufId(0),
                    old_off: 0,
                    old_len,
                    new_buf: BufId(1),
                    new_off: 0,
                    new_len,
                },
                10_000,
            )
            .expect("rename")
            .result,
            0
        );

        let dir_path = if prefix.is_empty() {
            "/"
        } else {
            prefix.trim_end_matches('/')
        };
        let dir_flags = OpenFlags {
            read: true,
            directory: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 22, FileSlot(1), BufId(0), dir_path, dir_flags),
            0
        );
        assert_eq!(
            submit_and_wait_one(
                &mut DynIo(io),
                &mut out,
                Token(23),
                Op::FileSync {
                    slot: FileSlot(1),
                    datasync: false,
                },
                10_000,
            )
            .expect("dir sync")
            .result,
            0
        );
        assert_eq!(close_slot(io, &mut out, 24, FileSlot(1)), 0);
    });
}

/// `T1-F11-3`: `FileReserve` (`keep_size=false` and `keep_size=true`) + `FileStatus` (`file_size`).
#[test]
fn t1_f11_3_reserve_extend_and_keep_size_statx() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}reserve.bin");
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
        let r1 = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(2),
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
            10_000,
        )
        .expect("reserve extend");
        assert_eq!(r1.result, 0);
        assert_eq!(
            stat_size(io, &mut out, 3, BufId(0), BufId(1), &path),
            Ok(4096)
        );

        d = DynIo(io);
        let r2 = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(4),
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 4096,
                len: 4096,
                keep_size: true,
            },
            10_000,
        )
        .expect("reserve keep_size");
        assert_eq!(r2.result, 0);
        assert_eq!(
            stat_size(io, &mut out, 5, BufId(0), BufId(1), &path),
            Ok(4096),
            "keep_size=true must not change logical file size"
        );

        d = DynIo(io);
        let rd = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(6),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4096,
                file_off: 0,
            },
            10_000,
        )
        .expect("read zeros");
        assert_eq!(rd.result, 4096);
        assert!(d.buf(BufId(1))[..4096].iter().all(|&b| b == 0));
        assert_eq!(close_slot(io, &mut out, 7, FileSlot(0)), 0);
    });
}
