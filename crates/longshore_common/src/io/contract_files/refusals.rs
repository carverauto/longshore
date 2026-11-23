// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared-check refusal tests for [`super`].

use super::{
    base_limits, close_file, drive_many, drive_one, open_file, open_op, read_op, write_op,
    write_path,
};
use crate::io::must::{must, Must};
use crate::io::{
    BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, EBADF, EBUSY,
    EINVAL,
};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    slot_busy_and_free_refusals(make, root);
    invalid_paths_and_status_buf(make, root);
    direct_alignment_and_punch(make, root);
    ready_list_drains_behind_timer(make);
}

fn slot_busy_and_free_refusals<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(8);
    let p1 = format!("{root}busy1.dat");
    let p2 = format!("{root}busy2.dat");
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &p1, rw),
        0
    );
    assert_eq!(
        open_file(&mut io, &mut out, 2, FileSlot(0), BufId(0), &p2, rw),
        -EBUSY
    );
    assert_eq!(close_file(&mut io, &mut out, 3, FileSlot(0)), 0);

    // Concurrent FileOpen into an Opening slot -> -EBUSY.
    let l1 = write_path(&mut io, BufId(0), 0, &p1);
    let l2 = write_path(&mut io, BufId(1), 0, &p2);
    io.must_submit(Token(4), open_op(FileSlot(0), BufId(0), l1, rw));
    io.must_submit(Token(5), open_op(FileSlot(0), BufId(1), l2, rw));
    let cs = drive_many(&mut io, &mut out, [Token(4), Token(5)]);
    assert_eq!(cs[0].result, 0);
    assert_eq!(cs[1].result, -EBUSY);
    assert_eq!(close_file(&mut io, &mut out, 6, FileSlot(0)), 0);

    // Ops on Free or OOB slot -> -EBADF.
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(7),
            Op::FileClose { slot: FileSlot(0) }
        ),
        -EBADF
    );
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(8),
            read_op(FileSlot(4), BufId(0), 8, 0),
        ),
        -EBADF
    );
}

fn invalid_paths_and_status_buf<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = base_limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    // Missing trailing NUL.
    io.buf(BufId(0))[..4].copy_from_slice(b"abcd");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(1),
            open_op(FileSlot(0), BufId(0), 4, rw),
        ),
        -EINVAL
    );
    // Embedded NUL.
    io.buf(BufId(0))[..5].copy_from_slice(b"ab\0c\0");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(2),
            open_op(FileSlot(0), BufId(0), 5, rw),
        ),
        -EINVAL
    );
    // Length past buffer.
    let small = limits.small(0);
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(3),
            open_op(FileSlot(0), small, limits.small_buffer_size + 1, rw),
        ),
        -EINVAL
    );
    // FileStatus output buffer < 256 bytes -> -EINVAL.
    let p = format!("{root}stat_small.dat");
    let plen = write_path(&mut io, BufId(0), 0, &p);
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(4),
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen,
                out_buf: small,
                out_off: 0,
            },
        ),
        -EINVAL
    );
}

fn direct_alignment_and_punch<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = base_limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let p = format!("{root}direct_bad.dat");
    let plen = write_path(&mut io, BufId(0), 0, &p);
    let df = OpenFlags {
        read: true,
        write: true,
        create: true,
        direct: true,
        ..Default::default()
    };
    // Misaligned write behind an Opening direct FileOpen -> -EINVAL.
    io.must_submit(Token(1), open_op(FileSlot(0), BufId(0), plen, df));
    io.must_submit(Token(2), write_op(FileSlot(0), BufId(1), 512, 0));
    let cs = drive_many(&mut io, &mut out, [Token(1), Token(2)]);
    assert_eq!(cs[1].result, -EINVAL);
    if cs[0].result == 0 {
        misaligned_direct_ops_refused(&mut io, &mut out, limits.aligned(0));
        assert_eq!(close_file(&mut io, &mut out, 6, FileSlot(0)), 0);
    }
}

/// Tokens 3-5: on an open direct file, a read into a misaligned buffer offset,
/// a write at a misaligned file offset, and a misaligned punch are refused.
fn misaligned_direct_ops_refused(io: &mut impl Io, out: &mut Completions, a: BufId) {
    assert_eq!(
        drive_one(
            io,
            out,
            Token(3),
            Op::FileRead {
                slot: FileSlot(0),
                buf: a,
                off: 512,
                len: 4096,
                file_off: 0,
            },
        ),
        -EINVAL
    );
    assert_eq!(
        drive_one(io, out, Token(4), write_op(FileSlot(0), a, 4096, 512)),
        -EINVAL
    );
    assert_eq!(
        drive_one(
            io,
            out,
            Token(5),
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 512,
                len: 4096,
            },
        ),
        -EINVAL
    );
}

fn ready_list_drains_behind_timer<IO: Io>(make: &mut impl FnMut(Limits) -> IO) {
    let mut io = make(base_limits());
    let mut out = Completions::new(8);
    io.must_submit(
        Token(1),
        Op::Timeout {
            nanos: 5_000_000_000,
        },
    );
    io.must_submit(Token(2), read_op(FileSlot(0), BufId(0), 16, 0));
    let t0 = std::time::Instant::now();
    io.must_tick(true, &mut out);
    assert!(t0.elapsed() < std::time::Duration::from_secs(1));
    let c = must(out.pop(), "ready completion");
    assert_eq!((c.token, c.result), (Token(2), -EBADF));
    io.must_submit(Token(3), Op::Cancel { target: Token(1) });
    let _: [Completion; 1] = drive_many(&mut io, &mut out, [Token(1)]);
}
