// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Functional contract tests covering all 11 file operations.

use super::{
    base_limits, close_file, drive_one, open_file, read_op, rw_create, stat_file, write_op,
};
use crate::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ENOENT};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root_dir: &str) {
    open_close_empty_excl(make, root_dir);
    offset_rw_hole_short_eof(make, root_dir);
    direct_io_aligned(make, root_dir);
    sync_reserve_punch(make, root_dir);
}

fn open_close_empty_excl<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(8);
    let p1 = format!("{root}f1.dat");
    let p2 = format!("{root}f2.dat");
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &p2, ro),
        -ENOENT
    );
    assert_eq!(
        open_file(&mut io, &mut out, 2, FileSlot(0), BufId(0), &p1, rw),
        0
    );
    io.buf(BufId(1))[..7].copy_from_slice(b"initial");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(3),
            write_op(FileSlot(0), BufId(1), 7, 0),
        ),
        7
    );
    assert_eq!(close_file(&mut io, &mut out, 4, FileSlot(0)), 0);

    let excl = OpenFlags {
        exclusive: true,
        ..rw
    };
    assert_eq!(
        open_file(&mut io, &mut out, 5, FileSlot(0), BufId(0), &p1, excl),
        -crate::io::posix::errno::linux::EEXIST
    );
    let empty = OpenFlags {
        read: true,
        write: true,
        empty: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 6, FileSlot(0), BufId(0), &p1, empty),
        0
    );
    assert_eq!(close_file(&mut io, &mut out, 7, FileSlot(0)), 0);
    assert_eq!(
        stat_file(&mut io, &mut out, 8, BufId(0), BufId(1), &p1),
        Ok(0)
    );
}

fn offset_rw_hole_short_eof<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(8);
    let path = format!("{root}rw_off.dat");
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &path, rw),
        0
    );
    io.buf(BufId(0))[..16].copy_from_slice(b"0123456789abcdef");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(2),
            write_op(FileSlot(0), BufId(0), 16, 100),
        ),
        16
    );
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(3),
            read_op(FileSlot(0), BufId(1), 16, 116),
        ),
        0
    );
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(4),
            read_op(FileSlot(0), BufId(1), 32, 108),
        ),
        8
    );
    assert_eq!(&io.buf(BufId(1))[..8], b"89abcdef");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(5),
            read_op(FileSlot(0), BufId(1), 16, 0),
        ),
        16
    );
    assert!(io.buf(BufId(1))[..16].iter().all(|&b| b == 0));
    assert_eq!(close_file(&mut io, &mut out, 6, FileSlot(0)), 0);
}

fn direct_io_aligned<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = base_limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let path = format!("{root}direct.dat");
    let flags = OpenFlags {
        read: true,
        write: true,
        create: true,
        direct: true,
        ..Default::default()
    };
    let open_res = open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &path, flags);
    if open_res == -crate::io::EINVAL {
        return;
    }
    assert_eq!(open_res, 0);
    let w = limits.aligned(0);
    let r = limits.aligned(1);
    assert_eq!(io.buf(w).as_ptr() as usize % limits.block_size as usize, 0);
    for (idx, b) in io.buf(w).iter_mut().enumerate() {
        *b = (idx.wrapping_mul(13) ^ 0x5A) as u8;
    }
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(2),
            write_op(FileSlot(0), w, 4096, 4096),
        ),
        4096
    );
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(3),
            read_op(FileSlot(0), r, 4096, 4096),
        ),
        4096
    );
    let (wb, rb) = io.buf_pair(w, r);
    assert_eq!(&rb[..4096], &wb[..4096]);
    assert_eq!(close_file(&mut io, &mut out, 4, FileSlot(0)), 0);
}

fn sync_reserve_punch<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(8);
    let path = format!("{root}space.dat");
    assert_eq!(
        open_file(
            &mut io,
            &mut out,
            1,
            FileSlot(0),
            BufId(0),
            &path,
            rw_create()
        ),
        0
    );
    reserve_sets_or_keeps_size(&mut io, &mut out, &path);
    write_sync_punch(&mut io, &mut out, &path);
    assert_eq!(close_file(&mut io, &mut out, 11, FileSlot(0)), 0);
}

/// Tokens 2-5: reserving grows the file, and reserving with `keep_size` does not.
fn reserve_sets_or_keeps_size(io: &mut impl Io, out: &mut Completions, path: &str) {
    assert_eq!(
        drive_one(
            io,
            out,
            Token(2),
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 8192,
                keep_size: false,
            },
        ),
        0
    );
    assert_eq!(stat_file(io, out, 3, BufId(0), BufId(1), path), Ok(8192));
    assert_eq!(
        drive_one(
            io,
            out,
            Token(4),
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 8192,
                len: 4096,
                keep_size: true,
            },
        ),
        0
    );
    assert_eq!(stat_file(io, out, 5, BufId(0), BufId(1), path), Ok(8192));
}

/// Tokens 6-10: write and sync the file, punch its second block, and read zeros
/// there with the size unchanged (macOS may not support punching).
fn write_sync_punch(io: &mut impl Io, out: &mut Completions, path: &str) {
    io.buf(BufId(0))[..8192].fill(0xAB);
    assert_eq!(
        drive_one(io, out, Token(6), write_op(FileSlot(0), BufId(0), 8192, 0)),
        8192
    );
    assert_eq!(
        drive_one(
            io,
            out,
            Token(7),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
        0
    );
    let p = drive_one(
        io,
        out,
        Token(8),
        Op::FilePunch {
            slot: FileSlot(0),
            file_off: 4096,
            len: 4096,
        },
    );
    if cfg!(target_os = "macos") && p == -crate::io::posix::errno::linux::EOPNOTSUPP {
        return;
    }
    assert_eq!(p, 0);
    assert_eq!(stat_file(io, out, 9, BufId(0), BufId(1), path), Ok(8192));
    assert_eq!(
        drive_one(io, out, Token(10), read_op(FileSlot(0), BufId(1), 8192, 0)),
        8192
    );
    assert!(io.buf(BufId(1))[..4096].iter().all(|&b| b == 0xAB));
    assert!(io.buf(BufId(1))[4096..].iter().all(|&b| b == 0));
}
