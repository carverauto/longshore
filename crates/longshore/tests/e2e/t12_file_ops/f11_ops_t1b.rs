// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 11 (Part B): `FilePunch`, `FileMkdir`, `FileRename`, `FileUnlink`, `FileStatus`.

use super::super::harness::{
    read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{
    close_slot, file_ops_limits, open_slot, run_on_backends, stat_size, DynIo, LINUX_EOPNOTSUPP,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token, ENOENT};

/// `T1-F11-4`: `FilePunch` zeros block-aligned range while preserving file size.
#[test]
fn t1_f11_4_punch_hole_zeros_preserving_size() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}punch.bin");
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
        d.buf(BufId(0))[..8192].fill(0x7E);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(0),
                    off: 0,
                    len: 8192,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result,
            8192
        );

        let punch = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(3),
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 4096,
                len: 4096,
            },
            10_000,
        )
        .expect("punch");

        if cfg!(target_os = "macos") && punch.result == -LINUX_EOPNOTSUPP {
            assert_eq!(close_slot(io, &mut out, 4, FileSlot(0)), 0);
            return;
        }
        assert_eq!(punch.result, 0);
        assert_eq!(
            stat_size(io, &mut out, 4, BufId(0), BufId(1), &path),
            Ok(8192)
        );
        let mut d = DynIo(io);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(5),
                Op::FileRead {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 8192,
                    file_off: 0,
                },
                10_000,
            )
            .expect("read")
            .result,
            8192
        );
        let got = read_pool_bytes(&mut d, BufId(1), 0, 8192);
        assert!(got[..4096].iter().all(|&b| b == 0x7E));
        assert!(got[4096..].iter().all(|&b| b == 0));
        assert_eq!(close_slot(io, &mut out, 6, FileSlot(0)), 0);
    });
}

/// `T1-F11-5`: `FileMkdir`, `FileRename` over existing file, `FileStatus`, and `FileUnlink`.
#[test]
fn t1_f11_5_mkdir_rename_over_unlink_status() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let sub = format!("{prefix}sub");
        let f_src = format!("{sub}/src.dat");
        let f_dst = format!("{sub}/dst.dat");
        let mut d = DynIo(io);
        let sub_len = write_nul_path(&mut d, BufId(0), 0, &sub);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(1),
                Op::FileMkdir {
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: sub_len,
                },
                10_000,
            )
            .expect("mkdir")
            .result,
            0
        );

        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 2, FileSlot(0), BufId(0), &f_src, rw),
            0
        );
        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(1), 0, b"winner-content");
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(3),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 14,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write src")
            .result,
            14
        );
        assert_eq!(close_slot(io, &mut out, 4, FileSlot(0)), 0);

        assert_eq!(
            open_slot(io, &mut out, 5, FileSlot(0), BufId(0), &f_dst, rw),
            0
        );
        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(1), 0, b"old");
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(6),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 3,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write dst")
            .result,
            3
        );
        assert_eq!(close_slot(io, &mut out, 7, FileSlot(0)), 0);

        let mut d = DynIo(io);
        let l_old = write_nul_path(&mut d, BufId(0), 0, &f_src);
        let l_new = write_nul_path(&mut d, BufId(1), 0, &f_dst);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(8),
                Op::FileRename {
                    old_buf: BufId(0),
                    old_off: 0,
                    old_len: l_old,
                    new_buf: BufId(1),
                    new_off: 0,
                    new_len: l_new,
                },
                10_000,
            )
            .expect("rename")
            .result,
            0
        );
        assert_eq!(
            stat_size(io, &mut out, 9, BufId(0), BufId(1), &f_src),
            Err(-ENOENT)
        );
        assert_eq!(
            stat_size(io, &mut out, 10, BufId(0), BufId(1), &f_dst),
            Ok(14)
        );

        let ro = OpenFlags {
            read: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 11, FileSlot(0), BufId(0), &f_dst, ro),
            0
        );
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
                    len: 14,
                    file_off: 0,
                },
                10_000,
            )
            .expect("read renamed")
            .result,
            14
        );
        assert_eq!(read_pool_bytes(&mut d, BufId(1), 0, 14), b"winner-content");
        assert_eq!(close_slot(io, &mut out, 13, FileSlot(0)), 0);

        let mut d = DynIo(io);
        let l_un = write_nul_path(&mut d, BufId(0), 0, &f_dst);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(14),
                Op::FileUnlink {
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: l_un,
                },
                10_000,
            )
            .expect("unlink")
            .result,
            0
        );
        assert_eq!(
            stat_size(io, &mut out, 15, BufId(0), BufId(1), &f_dst),
            Err(-ENOENT)
        );
    });
}
