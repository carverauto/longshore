// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Generic `IO: Io` contract suite for all 11 file operations and shared refusals.

use crate::io::must::{fail, Must};
use crate::io::{
    file_size, BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ENOENT,
};

mod ops;
mod refusals;

pub(crate) fn base_limits() -> Limits {
    Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 8192,
        small_buffers: 2,
        small_buffer_size: 128,
        files_max: 4,
        aligned_buffers: 2,
        aligned_buffer_size: 8192,
        block_size: 4096,
        ops_max: 16,
        ..Limits::BASE
    }
}

pub(crate) fn write_path(io: &mut impl Io, buf: BufId, off: u32, path: &str) -> u32 {
    let b = path.as_bytes();
    let dst = &mut io.buf(buf)[off as usize..off as usize + b.len() + 1];
    dst[..b.len()].copy_from_slice(b);
    dst[b.len()] = 0;
    (b.len() + 1) as u32
}

pub(crate) fn drive_one(io: &mut impl Io, out: &mut Completions, token: Token, op: Op) -> i32 {
    io.must_submit(token, op);
    for _ in 0..10_000 {
        io.must_tick(true, out);
        while let Some(c) = out.pop() {
            if c.token == token {
                return c.result;
            }
        }
    }
    fail(format_args!("timed out waiting for {token:?}"));
}

pub(crate) fn drive_many<const N: usize>(
    io: &mut impl Io,
    out: &mut Completions,
    tokens: [Token; N],
) -> [Completion; N] {
    let mut found: [Option<Completion>; N] = [None; N];
    for _ in 0..10_000 {
        io.must_tick(true, out);
        while let Some(c) = out.pop() {
            if let Some(pos) = tokens.iter().position(|&t| t == c.token) {
                found[pos] = Some(c);
            }
        }
        if found.iter().all(Option::is_some) {
            return found.map(Option::unwrap);
        }
    }
    fail(format_args!("timed out waiting for completions"));
}

pub(crate) fn open_file(
    io: &mut impl Io,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    buf: BufId,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    let path_len = write_path(io, buf, 0, path);
    drive_one(
        io,
        out,
        Token(tok),
        Op::FileOpen {
            slot,
            path_buf: buf,
            path_off: 0,
            path_len,
            flags,
        },
    )
}

pub(crate) fn close_file(io: &mut impl Io, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    drive_one(io, out, Token(tok), Op::FileClose { slot })
}

pub(crate) fn stat_file(
    io: &mut impl Io,
    out: &mut Completions,
    tok: u64,
    path_buf: BufId,
    out_buf: BufId,
    path: &str,
) -> Result<u64, i32> {
    let path_len = write_path(io, path_buf, 0, path);
    let r = drive_one(
        io,
        out,
        Token(tok),
        Op::FileStatus {
            path_buf,
            path_off: 0,
            path_len,
            out_buf,
            out_off: 0,
        },
    );
    if r < 0 {
        Err(r)
    } else {
        Ok(file_size(io.buf(out_buf)))
    }
}

/// Run the full shared file-operations contract suite against any [`Io`] backend.
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root_dir: &str) {
    ops::run_all(make, root_dir);
    mkdir_rename_unlink_status(make, root_dir);
    refusals::run_all(make, root_dir);
}

fn mkdir_rename_unlink_status<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(8);
    let sub = format!("{root}sub");
    let src = format!("{sub}/src.dat");
    let dst = format!("{sub}/dst.dat");
    let sl = write_path(&mut io, BufId(0), 0, &sub);
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(1),
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: sl,
            },
        ),
        0
    );
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 2, FileSlot(0), BufId(0), &src, rw),
        0
    );
    io.buf(BufId(1))[..6].copy_from_slice(b"kept!!");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(3),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 6,
                file_off: 0,
            },
        ),
        6
    );
    assert_eq!(close_file(&mut io, &mut out, 4, FileSlot(0)), 0);
    assert_eq!(
        open_file(&mut io, &mut out, 5, FileSlot(0), BufId(0), &dst, rw),
        0
    );
    let old_len = write_path(&mut io, BufId(0), 0, &src);
    let new_len = write_path(&mut io, BufId(1), 0, &dst);
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(6),
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len,
                new_buf: BufId(1),
                new_off: 0,
                new_len,
            },
        ),
        0
    );
    assert_eq!(
        stat_file(&mut io, &mut out, 7, BufId(0), BufId(1), &src),
        Err(-ENOENT)
    );
    assert_eq!(
        stat_file(&mut io, &mut out, 8, BufId(0), BufId(1), &dst),
        Ok(6)
    );
    assert_eq!(close_file(&mut io, &mut out, 9, FileSlot(0)), 0);

    let dir_path = if root.is_empty() {
        "/"
    } else {
        root.trim_end_matches('/')
    };
    let df = OpenFlags {
        read: true,
        directory: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 10, FileSlot(1), BufId(0), dir_path, df),
        0
    );
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(11),
            Op::FileSync {
                slot: FileSlot(1),
                datasync: false,
            },
        ),
        0
    );
    assert_eq!(close_file(&mut io, &mut out, 12, FileSlot(1)), 0);

    let ul = write_path(&mut io, BufId(0), 0, &dst);
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(13),
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        0
    );
    assert_eq!(
        stat_file(&mut io, &mut out, 14, BufId(0), BufId(1), &dst),
        Err(-ENOENT)
    );
}
