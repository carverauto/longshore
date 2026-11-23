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

/// Read-write flags that create the file if it is missing.
pub(crate) fn rw_create() -> OpenFlags {
    OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    }
}

/// An open of the path at the start of `path_buf` into `slot`.
pub(crate) fn open_op(slot: FileSlot, path_buf: BufId, path_len: u32, flags: OpenFlags) -> Op {
    Op::FileOpen {
        slot,
        path_buf,
        path_off: 0,
        path_len,
        flags,
    }
}

/// A write of `len` bytes from the start of `buf` at `file_off`.
pub(crate) fn write_op(slot: FileSlot, buf: BufId, len: u32, file_off: u64) -> Op {
    Op::FileWrite {
        slot,
        buf,
        off: 0,
        len,
        file_off,
    }
}

/// A read of `len` bytes from `file_off` into the start of `buf`.
pub(crate) fn read_op(slot: FileSlot, buf: BufId, len: u32, file_off: u64) -> Op {
    Op::FileRead {
        slot,
        buf,
        off: 0,
        len,
        file_off,
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
    mkdir_and_write_src(&mut io, &mut out, &sub, &src);
    rename_over_open_dst(&mut io, &mut out, &src, &dst);
    sync_root_dir(&mut io, &mut out, root);
    unlink_dst(&mut io, &mut out, &dst);
}

/// Tokens 1-4: make `sub`, then create `src` in it holding six bytes.
fn mkdir_and_write_src(io: &mut impl Io, out: &mut Completions, sub: &str, src: &str) {
    let sl = write_path(io, BufId(0), 0, sub);
    assert_eq!(
        drive_one(
            io,
            out,
            Token(1),
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: sl,
            },
        ),
        0
    );
    assert_eq!(
        open_file(io, out, 2, FileSlot(0), BufId(0), src, rw_create()),
        0
    );
    io.buf(BufId(1))[..6].copy_from_slice(b"kept!!");
    assert_eq!(
        drive_one(
            io,
            out,
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
    assert_eq!(close_file(io, out, 4, FileSlot(0)), 0);
}

/// Tokens 5-9: rename `src` over an open `dst`; `src` is gone and `dst` has its bytes.
fn rename_over_open_dst(io: &mut impl Io, out: &mut Completions, src: &str, dst: &str) {
    assert_eq!(
        open_file(io, out, 5, FileSlot(0), BufId(0), dst, rw_create()),
        0
    );
    let old_len = write_path(io, BufId(0), 0, src);
    let new_len = write_path(io, BufId(1), 0, dst);
    assert_eq!(
        drive_one(
            io,
            out,
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
    assert_eq!(stat_file(io, out, 7, BufId(0), BufId(1), src), Err(-ENOENT));
    assert_eq!(stat_file(io, out, 8, BufId(0), BufId(1), dst), Ok(6));
    assert_eq!(close_file(io, out, 9, FileSlot(0)), 0);
}

/// Tokens 10-12: open the root directory and sync it.
fn sync_root_dir(io: &mut impl Io, out: &mut Completions, root: &str) {
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
        open_file(io, out, 10, FileSlot(1), BufId(0), dir_path, df),
        0
    );
    assert_eq!(
        drive_one(
            io,
            out,
            Token(11),
            Op::FileSync {
                slot: FileSlot(1),
                datasync: false,
            },
        ),
        0
    );
    assert_eq!(close_file(io, out, 12, FileSlot(1)), 0);
}

/// Tokens 13-14: unlink `dst`; it no longer exists.
fn unlink_dst(io: &mut impl Io, out: &mut Completions, dst: &str) {
    let ul = write_path(io, BufId(0), 0, dst);
    assert_eq!(
        drive_one(
            io,
            out,
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
        stat_file(io, out, 14, BufId(0), BufId(1), dst),
        Err(-ENOENT)
    );
}
