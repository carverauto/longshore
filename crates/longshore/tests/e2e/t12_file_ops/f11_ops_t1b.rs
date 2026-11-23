// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 11 (Part B): `FilePunch`, `FileMkdir`, `FileRename`, `FileUnlink`, `FileStatus`.

use super::super::harness::{read_pool_bytes, write_nul_path, write_pool_bytes};
use super::helpers::{
    close_slot, file_ops_limits, open_slot, read_at, rename, run_on_backends, run_op, rw_create,
    stat_size, write_at, DynIo, LINUX_EOPNOTSUPP,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, ENOENT};

/// `T1-F11-4`: `FilePunch` zeros block-aligned range while preserving file size.
#[test]
fn t1_f11_4_punch_hole_zeros_preserving_size() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}punch.bin");
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, rw_create()),
            0
        );

        DynIo(io).buf(BufId(0))[..8192].fill(0x7E);
        let write = write_at(FileSlot(0), BufId(0), 8192, 0);
        assert_eq!(run_op(io, &mut out, 2, write, "write"), 8192);

        let punch = Op::FilePunch {
            slot: FileSlot(0),
            file_off: 4096,
            len: 4096,
        };
        let punched = run_op(io, &mut out, 3, punch, "punch");
        if cfg!(target_os = "macos") && punched == -LINUX_EOPNOTSUPP {
            assert_eq!(close_slot(io, &mut out, 4, FileSlot(0)), 0);
            return;
        }
        assert_eq!(punched, 0);
        assert_eq!(
            stat_size(io, &mut out, 4, BufId(0), BufId(1), &path),
            Ok(8192)
        );
        let read = read_at(FileSlot(0), BufId(1), 8192, 0);
        assert_eq!(run_op(io, &mut out, 5, read, "read"), 8192);
        let got = read_pool_bytes(&mut DynIo(io), BufId(1), 0, 8192);
        assert!(got[..4096].iter().all(|&b| b == 0x7E));
        assert!(got[4096..].iter().all(|&b| b == 0));
        assert_eq!(close_slot(io, &mut out, 6, FileSlot(0)), 0);
    });
}

/// Creates `path` with `bytes` through slot 0 using tokens `tok..tok + 3`.
fn create_with(io: &mut dyn Io, out: &mut Completions, tok: u64, path: &str, bytes: &[u8]) {
    assert_eq!(
        open_slot(io, out, tok, FileSlot(0), BufId(0), path, rw_create()),
        0
    );
    write_pool_bytes(&mut DynIo(io), BufId(1), 0, bytes);
    let len = u32::try_from(bytes.len()).unwrap();
    let write = write_at(FileSlot(0), BufId(1), len, 0);
    assert_eq!(
        run_op(io, out, tok + 1, write, "write"),
        i32::try_from(len).unwrap()
    );
    assert_eq!(close_slot(io, out, tok + 2, FileSlot(0)), 0);
}

/// Stages `path` in buffer 0 and runs the path-only op `make` builds.
fn path_op(
    io: &mut dyn Io,
    out: &mut Completions,
    tok: u64,
    path: &str,
    make: fn(u32) -> Op,
    what: &str,
) -> i32 {
    let len = write_nul_path(&mut DynIo(io), BufId(0), 0, path);
    run_op(io, out, tok, make(len), what)
}

/// Reads `f_dst` back and checks it holds the renamed source's bytes.
fn assert_holds_winner(io: &mut dyn Io, out: &mut Completions, f_dst: &str) {
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    assert_eq!(open_slot(io, out, 11, FileSlot(0), BufId(0), f_dst, ro), 0);
    let read = read_at(FileSlot(0), BufId(1), 14, 0);
    assert_eq!(run_op(io, out, 12, read, "read renamed"), 14);
    assert_eq!(
        read_pool_bytes(&mut DynIo(io), BufId(1), 0, 14),
        b"winner-content"
    );
    assert_eq!(close_slot(io, out, 13, FileSlot(0)), 0);
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
        let mkdir = |path_len| Op::FileMkdir {
            path_buf: BufId(0),
            path_off: 0,
            path_len,
        };
        assert_eq!(path_op(io, &mut out, 1, &sub, mkdir, "mkdir"), 0);

        create_with(io, &mut out, 2, &f_src, b"winner-content");
        create_with(io, &mut out, 5, &f_dst, b"old");

        assert_eq!(rename(io, &mut out, 8, &f_src, &f_dst), 0);
        assert_eq!(
            stat_size(io, &mut out, 9, BufId(0), BufId(1), &f_src),
            Err(-ENOENT)
        );
        assert_eq!(
            stat_size(io, &mut out, 10, BufId(0), BufId(1), &f_dst),
            Ok(14)
        );

        assert_holds_winner(io, &mut out, &f_dst);

        let unlink = |path_len| Op::FileUnlink {
            path_buf: BufId(0),
            path_off: 0,
            path_len,
        };
        assert_eq!(path_op(io, &mut out, 14, &f_dst, unlink, "unlink"), 0);
        assert_eq!(
            stat_size(io, &mut out, 15, BufId(0), BufId(1), &f_dst),
            Err(-ENOENT)
        );
    });
}
