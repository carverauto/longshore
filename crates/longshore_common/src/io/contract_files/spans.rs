// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The gathered write (`Op::FileWriteSpans`): spans of several buffers land
//! in the file in list order, buffered and with direct I/O, and a list with
//! more spans than the backend was built for is refused.

use super::{base_limits, close_file, drive_one, open_file, read_op, rw_create};
use crate::io::{BufId, Completions, FileSlot, Io, Limits, Op, Token, ECANCELED, EINVAL, ENOBUFS};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root_dir: &str) {
    buffered_spans_in_list_order(make, root_dir);
    direct_spans_of_aligned_buffers(make, root_dir);
    more_spans_than_built_for(make, root_dir);
    more_writes_than_built_for(make, root_dir);
}

/// Tokens 1-12: with room for two gathered writes in flight, a third is
/// refused, alone and as the third of a chain, whose members before it are
/// cancelled; once the two are done, two more may go.
fn more_writes_than_built_for<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let path = format!("{root}spans_many.dat");
    let opened = open_file(
        &mut io,
        &mut out,
        1,
        FileSlot(0),
        BufId(0),
        &path,
        rw_create(),
    );
    assert_eq!(opened, 0);
    let spans = [(BufId(1), 0, 8)];
    let list = limits.small(0);
    put_list(&mut io, list, &spans);
    let write = |at: u64| spans_op(FileSlot(0), list, &spans, at);
    let chain = [
        (Token(2), write(0)),
        (Token(3), write(8)),
        (Token(4), write(16)),
    ];
    assert!(io.submit_chain(&chain).is_ok());
    let got = results(&mut io, &mut out, 2);
    assert_eq!(got, [-ECANCELED, -ECANCELED, -ENOBUFS], "a chain of three");
    for k in 0..3u64 {
        assert!(io.submit(Token(5 + k), write(8 * k)).is_ok());
    }
    assert_eq!(
        results(&mut io, &mut out, 5),
        [8, 8, -ENOBUFS],
        "three alone"
    );
    let pair = [(Token(8), write(24)), (Token(9), write(32))];
    assert!(io.submit_chain(&pair).is_ok());
    let both = results(&mut io, &mut out, 8);
    assert_eq!(both[..2], [8, 8], "a chain of two, once the rows are free");
    assert_eq!(close_file(&mut io, &mut out, 10, FileSlot(0)), 0);
}

/// The results of the operations of tokens `first` on, up to three, as
/// they complete; no operation here completes with 0.
fn results(io: &mut impl Io, out: &mut Completions, first: u64) -> [i32; 3] {
    let mut got = [0i32; 3];
    let want = if first == 8 { 2 } else { 3 };
    while got[..want].contains(&0) {
        assert!(io.tick(true, out).is_ok());
        while let Some(c) = out.pop() {
            got[(c.token().0 - first) as usize] = c.result();
        }
    }
    got
}

fn limits() -> Limits {
    base_limits().with_gather(2, 4)
}

/// Put `spans` as a list at the start of `list`.
fn put_list(io: &mut impl Io, list: BufId, spans: &[(BufId, u32, u32)]) {
    let dst = io.buf(list);
    for (k, (buf, off, len)) in spans.iter().enumerate() {
        let words = [buf.0, *off, *len];
        for (w, word) in words.iter().enumerate() {
            let at = k * 12 + w * 4;
            dst[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
}

/// A gathered write of `spans`, listed at the start of `list`.
fn spans_op(slot: FileSlot, list: BufId, spans: &[(BufId, u32, u32)], file_off: u64) -> Op {
    Op::FileWriteSpans {
        slot,
        list,
        list_off: 0,
        count: spans.len() as u32,
        len: spans.iter().map(|s| s.2).sum(),
        file_off,
    }
}

/// Tokens 1-4: three spans of two buffers, one written from its middle.
fn buffered_spans_in_list_order<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let path = format!("{root}spans.dat");
    let opened = open_file(
        &mut io,
        &mut out,
        1,
        FileSlot(0),
        BufId(0),
        &path,
        rw_create(),
    );
    assert_eq!(opened, 0);
    io.buf(BufId(0))[..8].copy_from_slice(b"WXYZwxyz");
    io.buf(BufId(1))[100..105].copy_from_slice(b"hello");
    let spans = [(BufId(1), 100, 5), (BufId(0), 4, 3), (BufId(0), 0, 4)];
    let list = limits.small(0);
    put_list(&mut io, list, &spans);
    let op = spans_op(FileSlot(0), list, &spans, 10);
    assert_eq!(drive_one(&mut io, &mut out, Token(2), op), 12);
    let back = limits.aligned(0);
    let read = drive_one(
        &mut io,
        &mut out,
        Token(3),
        read_op(FileSlot(0), back, 64, 0),
    );
    assert_eq!(read, 22, "ten bytes of hole, then the twelve");
    assert_eq!(&io.buf(back)[..22], b"\0\0\0\0\0\0\0\0\0\0hellowxyWXYZ");
    assert_eq!(close_file(&mut io, &mut out, 4, FileSlot(0)), 0);
}

/// Tokens 1-5: whole blocks of two aligned buffers, with direct I/O, and
/// a span that is not whole blocks refused.
fn direct_spans_of_aligned_buffers<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let path = format!("{root}spans_direct.dat");
    let flags = rw_create().with_direct(true);
    let opened = open_file(&mut io, &mut out, 1, FileSlot(0), BufId(0), &path, flags);
    if opened == -EINVAL {
        // A file system without direct I/O.
        return;
    }
    assert_eq!(opened, 0);
    let (a, b) = (limits.aligned(0), limits.aligned(1));
    io.buf(a)[..4096].fill(b'a');
    io.buf(b)[..8192].fill(b'b');
    let spans = [(b, 4096, 4096), (a, 0, 4096)];
    let list = limits.small(0);
    put_list(&mut io, list, &spans);
    let op = spans_op(FileSlot(0), list, &spans, 4096);
    assert_eq!(drive_one(&mut io, &mut out, Token(2), op), 8192);
    let read = drive_one(
        &mut io,
        &mut out,
        Token(3),
        read_op(FileSlot(0), a, 8192, 4096),
    );
    assert_eq!(read, 8192);
    assert!(io.buf(a)[..4096].iter().all(|x| *x == b'b'));
    assert!(io.buf(a)[4096..8192].iter().all(|x| *x == b'a'));
    let ragged = [(b, 0, 4096), (b, 4096, 100)];
    put_list(&mut io, list, &ragged);
    let refused = spans_op(FileSlot(0), list, &ragged, 0);
    assert_eq!(drive_one(&mut io, &mut out, Token(4), refused), -EINVAL);
    assert_eq!(close_file(&mut io, &mut out, 5, FileSlot(0)), 0);
}

/// Tokens 1-3: five spans where the backend was built for four.
fn more_spans_than_built_for<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let limits = limits();
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let path = format!("{root}spans_long.dat");
    let opened = open_file(
        &mut io,
        &mut out,
        1,
        FileSlot(0),
        BufId(0),
        &path,
        rw_create(),
    );
    assert_eq!(opened, 0);
    let spans = [(BufId(1), 0, 1); 5];
    let list = limits.small(0);
    put_list(&mut io, list, &spans);
    let op = spans_op(FileSlot(0), list, &spans, 0);
    assert_eq!(drive_one(&mut io, &mut out, Token(2), op), -EINVAL);
    assert_eq!(close_file(&mut io, &mut out, 3, FileSlot(0)), 0);
}
