// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 11 (Part A): Open/Write/Read/Close, Sync, and Reserve + `statx`.

use super::super::harness::{read_pool_bytes, submit_and_wait_one, write_pool_bytes};
use super::helpers::{
    close_slot, file_ops_limits, open_slot, read_at, rename, run_on_backends, run_op, rw_create,
    stat_size, write_at, DynIo,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token};

/// `T1-F11-1`: `FileOpen` + `FileWrite` at offset 4096 + `FileRead` at offset 4096 + `FileClose`.
#[test]
fn t1_f11_1_open_write_read_close_at_offset() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}rw_offset.bin");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot::new(0), BufId::new(0), &path, rw),
            0
        );

        let pattern: Vec<u8> = (0..1024u32).map(|i| ((i * 17) ^ 0x3C) as u8).collect();
        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId::new(0), 0, &pattern);
        let wr = submit_and_wait_one(
            &mut d,
            &mut out,
            Token::new(2),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(0),
                off: 0,
                len: 1024,
                file_off: 4096,
            },
            10_000,
        )
        .expect("write");
        assert_eq!(wr.result(), 1024);

        let rd = submit_and_wait_one(
            &mut d,
            &mut out,
            Token::new(3),
            Op::FileRead {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 1024,
                file_off: 4096,
            },
            10_000,
        )
        .expect("read");
        assert_eq!(rd.result(), 1024);
        assert_eq!(read_pool_bytes(&mut d, BufId::new(1), 0, 1024), pattern);
        assert_eq!(close_slot(io, &mut out, 4, FileSlot::new(0)), 0);
    });
}

/// Writes ten bytes to `slot` 0, then syncs it with `datasync` true and false.
fn write_then_sync_both_ways(io: &mut dyn Io, out: &mut Completions) {
    write_pool_bytes(&mut DynIo(io), BufId::new(1), 0, b"sync-bytes");
    let write = write_at(FileSlot::new(0), BufId::new(1), 10, 0);
    assert_eq!(run_op(io, out, 2, write, "write"), 10);
    for (idx, datasync) in [true, false].into_iter().enumerate() {
        let sync = Op::FileSync {
            slot: FileSlot::new(0),
            datasync,
        };
        assert_eq!(run_op(io, out, 10 + idx as u64, sync, "sync"), 0);
    }
}

/// Opens the directory holding the test files and syncs it.
fn sync_parent_directory(io: &mut dyn Io, out: &mut Completions, prefix: &str) {
    let dir_path = if prefix.is_empty() {
        "/"
    } else {
        prefix.trim_end_matches('/')
    };
    let dir_flags = OpenFlags::NONE.with_read(true).with_directory(true);
    assert_eq!(
        open_slot(
            io,
            out,
            22,
            FileSlot::new(1),
            BufId::new(0),
            dir_path,
            dir_flags
        ),
        0
    );
    let sync = Op::FileSync {
        slot: FileSlot::new(1),
        datasync: false,
    };
    assert_eq!(run_op(io, out, 23, sync, "dir sync"), 0);
    assert_eq!(close_slot(io, out, 24, FileSlot::new(1)), 0);
}

/// `T1-F11-2`: `FileSync` (both `datasync=true/false`) on a regular file and a directory.
#[test]
fn t1_f11_2_sync_file_and_directory_datasync() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let p_old = format!("{prefix}sync_old.bin");
        let p_new = format!("{prefix}sync_new.bin");
        assert_eq!(
            open_slot(
                io,
                &mut out,
                1,
                FileSlot::new(0),
                BufId::new(0),
                &p_old,
                rw_create()
            ),
            0
        );
        write_then_sync_both_ways(io, &mut out);
        assert_eq!(close_slot(io, &mut out, 20, FileSlot::new(0)), 0);
        assert_eq!(rename(io, &mut out, 21, &p_old, &p_new), 0);
        sync_parent_directory(io, &mut out, prefix);
    });
}

fn reserve(file_off: u64, keep_size: bool) -> Op {
    Op::FileReserve {
        slot: FileSlot::new(0),
        file_off,
        len: 4096,
        keep_size,
    }
}

/// `T1-F11-3`: `FileReserve` (`keep_size=false` and `keep_size=true`) + `FileStatus` (`file_size`).
#[test]
fn t1_f11_3_reserve_extend_and_keep_size_statx() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}reserve.bin");
        assert_eq!(
            open_slot(
                io,
                &mut out,
                1,
                FileSlot::new(0),
                BufId::new(0),
                &path,
                rw_create()
            ),
            0
        );

        let r1 = run_op(io, &mut out, 2, reserve(0, false), "reserve extend");
        assert_eq!(r1, 0);
        assert_eq!(
            stat_size(io, &mut out, 3, BufId::new(0), BufId::new(1), &path),
            Ok(4096)
        );

        let r2 = run_op(io, &mut out, 4, reserve(4096, true), "reserve keep_size");
        assert_eq!(r2, 0);
        assert_eq!(
            stat_size(io, &mut out, 5, BufId::new(0), BufId::new(1), &path),
            Ok(4096),
            "keep_size=true must not change logical file size"
        );

        let read = read_at(FileSlot::new(0), BufId::new(1), 4096, 0);
        assert_eq!(run_op(io, &mut out, 6, read, "read zeros"), 4096);
        assert!(DynIo(io).buf(BufId::new(1))[..4096].iter().all(|&b| b == 0));
        assert_eq!(close_slot(io, &mut out, 7, FileSlot::new(0)), 0);
    });
}
