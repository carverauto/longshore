// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Generic `IO: Io` contract suite for [`Io::submit_chain`].

use crate::io::contract_files::{base_limits, close_file, drive_one, open_file, write_path};
use crate::io::must::{fail, Must};
use crate::io::{
    BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ECANCELED, ENOENT,
};

mod cases;

pub(super) fn collect_chain<const N: usize>(
    io: &mut impl Io,
    out: &mut Completions,
    ops: [(Token, Op); N],
) -> Vec<Completion> {
    io.must_submit_chain(&ops);
    let mut got = Vec::with_capacity(N);
    for _ in 0..10_000 {
        io.must_tick(true, out);
        while let Some(c) = out.pop() {
            if ops.iter().any(|(t, _)| *t == c.token) {
                got.push(c);
            }
        }
        if got.len() == N {
            return got;
        }
    }
    fail(format_args!("timed out collecting chain"));
}

/// Run all 7 linked-chain contract tests against any [`Io`] backend.
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root_dir: &str) {
    write_then_sync(make, root_dir);
    short_read_cancels_tail(make, root_dir);
    failure_at_each_position(make, root_dir);
    cases::run_all(make, root_dir);
}

fn write_then_sync<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let p = format!("{root}c_ws.dat");
    let plen = write_path(&mut io, BufId(0), 0, &p);
    io.buf(BufId(1))[..13].copy_from_slice(b"chain-payload");
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    let cs = collect_chain(
        &mut io,
        &mut out,
        [
            (
                Token(1),
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: plen,
                    flags: rw,
                },
            ),
            (
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 13,
                    file_off: 0,
                },
            ),
            (
                Token(3),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
            (Token(4), Op::FileClose { slot: FileSlot(0) }),
        ],
    );
    let res: Vec<(u64, i32)> = cs.iter().map(|c| (c.token.0, c.result)).collect();
    assert_eq!(res, vec![(1, 0), (2, 13), (3, 0), (4, 0)]);

    assert_eq!(
        open_file(&mut io, &mut out, 5, FileSlot(0), BufId(0), &p, rw),
        0
    );
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(6),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 13,
                file_off: 0,
            },
        ),
        13
    );
    assert_eq!(&io.buf(BufId(0))[..13], b"chain-payload");
    assert_eq!(close_file(&mut io, &mut out, 7, FileSlot(0)), 0);
}

fn short_read_cancels_tail<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let p = format!("{root}c_short.dat");
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &p, rw),
        0
    );
    io.buf(BufId(0))[..5].copy_from_slice(b"short");
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 5,
                file_off: 0,
            },
        ),
        5
    );
    let cs = collect_chain(
        &mut io,
        &mut out,
        [
            (
                Token(3),
                Op::FileRead {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 64,
                    file_off: 0,
                },
            ),
            (
                Token(4),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ],
    );
    assert_eq!((cs[0].token, cs[0].result), (Token(3), 5));
    assert_eq!((cs[1].token, cs[1].result), (Token(4), -ECANCELED));
    assert_eq!(close_file(&mut io, &mut out, 5, FileSlot(0)), 0);
}

fn failure_at_each_position<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let ok_path = format!("{root}c_ok.dat");
    let missing = format!("{root}c_missing.dat");
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    let ro = OpenFlags::NONE.with_read(true);
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &ok_path, rw),
        0
    );
    let mlen = write_path(&mut io, BufId(0), 0, &missing);
    io.buf(BufId(1))[..4].copy_from_slice(b"test");
    let fail = Op::FileOpen {
        slot: FileSlot(1),
        path_buf: BufId(0),
        path_off: 0,
        path_len: mlen,
        flags: ro,
    };
    let wr = Op::FileWrite {
        slot: FileSlot(0),
        buf: BufId(1),
        off: 0,
        len: 4,
        file_off: 0,
    };
    let sy = Op::FileSync {
        slot: FileSlot(0),
        datasync: true,
    };

    let c0 = collect_chain(
        &mut io,
        &mut out,
        [(Token(10), fail), (Token(11), wr), (Token(12), sy)],
    );
    assert_eq!(
        (c0[0].result, c0[1].result, c0[2].result),
        (-ENOENT, -ECANCELED, -ECANCELED)
    );

    let c1 = collect_chain(
        &mut io,
        &mut out,
        [(Token(20), wr), (Token(21), fail), (Token(22), sy)],
    );
    assert_eq!(
        (c1[0].result, c1[1].result, c1[2].result),
        (4, -ENOENT, -ECANCELED)
    );

    let c2 = collect_chain(
        &mut io,
        &mut out,
        [(Token(30), wr), (Token(31), sy), (Token(32), fail)],
    );
    assert_eq!((c2[0].result, c2[1].result, c2[2].result), (4, 0, -ENOENT));
    assert_eq!(close_file(&mut io, &mut out, 40, FileSlot(0)), 0);
}
