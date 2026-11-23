// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Refusal, `Busy`, interleaving, and cancel cases for [`super`].

use super::collect_chain;
use crate::io::contract_files::{
    base_limits, close_file, drive_many, drive_one, open_file, rw_create, write_path,
};
use crate::io::must::Must;
use crate::io::{
    file_size, BufId, Busy, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock,
    Token, EBADF, ECANCELED, EINVAL, ENOENT,
};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root_dir: &str) {
    refusal_at_each_position(make, root_dir);
    atomic_busy_on_full_slots(make, root_dir);
    chain_among_other_ops(make, root_dir);
    cancel_unstarted_member(make, root_dir);
    pipelined_open_fail_then_close_open_chain(make, root_dir);
    aborted_reopen_preserves_direct_flag(make, root_dir);
}

fn refusal_at_each_position<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let p = format!("{root}c_refuse.dat");
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &p, rw),
        0
    );
    let wr = Op::FileWrite {
        slot: FileSlot(0),
        buf: BufId(0),
        off: 0,
        len: 4,
        file_off: 0,
    };
    let sy = Op::FileSync {
        slot: FileSlot(0),
        datasync: true,
    };
    let bad = Op::FilePunch {
        slot: FileSlot(0),
        file_off: 1,
        len: 4096,
    };
    for pos in 0..3 {
        let mut chain = [(Token(0), wr), (Token(1), wr), (Token(2), sy)];
        chain[pos].1 = bad;
        let cs = collect_chain(&mut io, &mut out, chain);
        for (i, c) in cs.iter().enumerate() {
            assert_eq!(c.result, if i == pos { -EINVAL } else { -ECANCELED });
        }
    }
    let plen = write_path(&mut io, BufId(0), 0, &p);
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(9),
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        0
    );
    assert_eq!(file_size(io.buf(BufId(1))), 0);
    assert_eq!(close_file(&mut io, &mut out, 10, FileSlot(0)), 0);
}

fn atomic_busy_on_full_slots<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut limits = base_limits();
    limits.ops_max = 3;
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let p = format!("{root}c_busy.dat");
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &p, rw),
        0
    );
    io.must_submit(Token(10), Op::Timeout { nanos: 100_000 });
    io.must_submit(Token(11), Op::Timeout { nanos: 100_000 });
    let chain = [
        (
            Token(20),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4,
                file_off: 0,
            },
        ),
        (
            Token(21),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ];
    assert_eq!(io.submit_chain(&chain), Err(Busy));
    let _: [Completion; 2] = drive_many(&mut io, &mut out, [Token(10), Token(11)]);
    assert_eq!(io.submit_chain(&[]), Ok(()));
    let bad_op = io.submit_chain(&[(Token(30), Op::Close { sock: Sock(0) })]);
    if bad_op.is_ok() {
        assert_eq!(
            drive_many(&mut io, &mut out, [Token(30)])[0].result,
            -EINVAL
        );
    }
    assert_eq!(close_file(&mut io, &mut out, 40, FileSlot(0)), 0);
}

fn chain_among_other_ops<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let missing = format!("{root}c_absent.dat");
    let plen = write_path(&mut io, BufId(0), 0, &missing);
    io.must_submit(Token(1), Op::Timeout { nanos: 100_000 });
    io.must_submit_chain(&[
        (
            Token(10),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    ..Default::default()
                },
            },
        ),
        (Token(11), Op::FileClose { slot: FileSlot(0) }),
    ]);
    io.must_submit(Token(2), Op::Timeout { nanos: 200_000 });
    let cs = drive_many(
        &mut io,
        &mut out,
        [Token(1), Token(2), Token(10), Token(11)],
    );
    assert_eq!(cs[0].result, 0);
    assert_eq!(cs[1].result, 0);
    assert_eq!(cs[2].result, -ENOENT);
    assert_eq!(cs[3].result, -ECANCELED);
}

fn cancel_unstarted_member<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let p = format!("{root}c_cancel.dat");
    let plen = write_path(&mut io, BufId(0), 0, &p);
    io.buf(BufId(1))[..8].copy_from_slice(b"12345678");
    io.must_submit_chain(&[
        (
            Token(10),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen,
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
                len: 8,
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
    ]);
    io.must_submit(Token(99), Op::Cancel { target: Token(12) });
    let cs = drive_many(
        &mut io,
        &mut out,
        [Token(10), Token(11), Token(12), Token(99)],
    );
    assert_eq!(cs[0].result, 0);
    assert_eq!(cs[1].result, 8);
    assert!(
        (cs[3].result < 0 && cs[2].result == 0)
            || (cs[3].result == 0 && cs[2].result == -ECANCELED)
    );
    assert_eq!(close_file(&mut io, &mut out, 20, FileSlot(0)), 0);
}

fn pipelined_open_fail_then_close_open_chain<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    root: &str,
) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let missing = format!("{root}c_pipe_missing.dat");
    let valid = format!("{root}c_pipe_valid.dat");
    let mlen = write_path(&mut io, BufId(0), 0, &missing);
    let vlen = write_path(&mut io, BufId(1), 0, &valid);
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };

    io.must_submit(
        Token(100),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: mlen,
            flags: ro,
        },
    );
    io.must_submit_chain(&[
        (Token(101), Op::FileClose { slot: FileSlot(0) }),
        (
            Token(102),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(1),
                path_off: 0,
                path_len: vlen,
                flags: rw,
            },
        ),
    ]);

    let cs = drive_many(&mut io, &mut out, [Token(100), Token(101), Token(102)]);
    assert_eq!(cs[0].result, -ENOENT);
    assert_eq!(cs[1].result, -EBADF);
    assert_eq!(cs[2].result, -ECANCELED);

    assert_eq!(
        open_file(&mut io, &mut out, 103, FileSlot(0), BufId(1), &valid, rw),
        0
    );
    assert_eq!(close_file(&mut io, &mut out, 104, FileSlot(0)), 0);
}

fn aborted_reopen_preserves_direct_flag<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut io = make(base_limits());
    let mut out = Completions::new(16);
    let f1 = format!("{root}c_direct_keep.dat");
    let f2 = format!("{root}c_direct_next.dat");
    let missing = format!("{root}c_direct_missing.dat");

    let rw_direct = OpenFlags {
        direct: true,
        ..rw_create()
    };
    assert_eq!(
        open_file(&mut io, &mut out, 2, FileSlot(0), BufId(0), &f1, rw_direct),
        0
    );
    let cs = aborted_buffered_reopen(&mut io, &mut out, &missing, &f2);
    assert_eq!(
        (cs[0].result, cs[1].result, cs[2].result),
        (-ENOENT, -ECANCELED, -ECANCELED)
    );
    // Slot 0 still holds the direct file, so an unaligned write is refused.
    assert_eq!(
        drive_one(
            &mut io,
            &mut out,
            Token(13),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 4,
                file_off: 0,
            },
        ),
        -EINVAL
    );
    assert_eq!(close_file(&mut io, &mut out, 14, FileSlot(0)), 0);
}

/// Tokens 10-12: a chain whose failed first open aborts closing slot 0 and
/// reopening it buffered (not direct) on `f2`.
fn aborted_buffered_reopen(
    io: &mut impl Io,
    out: &mut Completions,
    missing: &str,
    f2: &str,
) -> Vec<Completion> {
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    let rw_buf = OpenFlags {
        direct: false,
        ..rw_create()
    };
    let mlen = write_path(io, BufId(0), 0, missing);
    let f2len = write_path(io, BufId(1), 0, f2);
    collect_chain(
        io,
        out,
        [
            (
                Token(10),
                Op::FileOpen {
                    slot: FileSlot(1),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: mlen,
                    flags: ro,
                },
            ),
            (Token(11), Op::FileClose { slot: FileSlot(0) }),
            (
                Token(12),
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(1),
                    path_off: 0,
                    path_len: f2len,
                    flags: rw_buf,
                },
            ),
        ],
    )
}
