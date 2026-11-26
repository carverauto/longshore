// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A zero-copy send that says when its bytes were taken apart from when
//! its buffer is free again (`Op::SendZc`): a first report with more to
//! come, a last one once, the buffer as it was in between, and a last
//! report whatever ends the socket first.

use super::{close_socks, connect_one, drain_until, net_limits};
use crate::io::must::{fail, must_len, Must};
use crate::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

/// What a send reported: the bytes taken, and whether that came apart
/// from its end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sent {
    taken: i32,
    in_two: bool,
}

fn pattern(piece: usize) -> [u8; 64] {
    core::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ (piece as u8).wrapping_mul(29))
}

/// A send of what is left of `piece` from byte `from` on.
fn send_zc(sock: Sock, piece: usize, from: usize) -> Op {
    Op::SendZc {
        sock,
        buf: BufId(0),
        off: (piece * 64 + from) as u32,
        len: (64 - from) as u32,
    }
}

/// One report of a send: a first one says more follows and takes bytes;
/// a last one repeats what the first took, or is all there is.
fn note(seen: &mut Option<i32>, c: Completion) -> Option<Sent> {
    assert!(c.buf.is_none(), "a send names no buffer: {c:?}");
    match (*seen, c.more) {
        (None, true) => {
            assert!(c.result > 0, "a first report takes bytes: {c:?}");
            *seen = Some(c.result);
            None
        }
        (None, false) => Some(Sent {
            taken: c.result,
            in_two: false,
        }),
        (Some(taken), false) => {
            assert_eq!(c.result, taken, "the last report repeats the first");
            Some(Sent {
                taken,
                in_two: true,
            })
        }
        (Some(_), true) => fail(format_args!("a second first report: {c:?}")),
    }
}

/// Receive on `sock` until `want` bytes have come.
fn recv_exact<IO: Io>(io: &mut IO, out: &mut Completions, sock: Sock, want: usize) -> Vec<u8> {
    let mut got = Vec::with_capacity(want);
    while got.len() < want {
        let op = Op::Recv {
            sock,
            buf: BufId(1),
            off: 0,
            len: 256,
        };
        io.must_submit(Token(70), op);
        let mut result = 0;
        drain_until(io, out, |_, c| {
            result = c.result;
            c.token.0 == 70
        });
        assert!(result > 0, "receive before the end of the stream: {result}");
        got.extend_from_slice(&io.buf(BufId(1))[..must_len(result)]);
    }
    got
}

/// Several sends, each started as soon as the one before it was taken
/// (a send taken short is followed by one of the rest): every one ends
/// once, the peer reads the bytes in the order sent, and the buffer is as
/// it was until the last report.
fn the_next_send_follows_the_first_report<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    const PIECES: usize = 6;
    let mut io = make(net_limits());
    let mut out = Completions::new(8);
    let client = Sock(1);
    let server = connect_one(&mut io, &mut out, addr, Sock(0), client);
    for piece in 0..PIECES {
        io.buf(BufId(0))[piece * 64..][..64].copy_from_slice(&pattern(piece));
    }
    // By send: what its first report took, and its end.
    let (mut firsts, mut done) = (Vec::new(), Vec::new());
    for piece in 0..PIECES {
        let mut from = 0;
        while from < 64 {
            let send = firsts.len();
            firsts.push(None);
            done.push(None);
            io.must_submit(Token(100 + send as u64), send_zc(client, piece, from));
            // On when it was taken, not when its buffer is free.
            drain_until(&mut io, &mut out, |io, c| {
                let at = (c.token.0 - 100) as usize;
                assert!(done[at].is_none(), "a second last report: {c:?}");
                done[at] = note(&mut firsts[at], c);
                // The backend has not touched what it sends.
                assert_eq!(io.buf(BufId(0))[piece * 64..][..64], pattern(piece));
                firsts[send].is_some() || done[send].is_some()
            });
            let taken = firsts[send].or(done[send].map(|s: Sent| s.taken));
            from += must_len(taken.unwrap_or(0).max(0));
            assert!(taken.is_some_and(|n| n > 0), "the send failed: {taken:?}");
        }
    }
    while done.iter().any(Option::is_none) {
        drain_until(&mut io, &mut out, |_, c| {
            let at = (c.token.0 - 100) as usize;
            assert!(done[at].is_none(), "a second last report: {c:?}");
            done[at] = note(&mut firsts[at], c);
            true
        });
    }
    let got = recv_exact(&mut io, &mut out, server, PIECES * 64);
    for piece in 0..PIECES {
        assert_eq!(got[piece * 64..][..64], pattern(piece), "piece {piece}");
    }
    room_for_every_operation(&mut io, &mut out);
    close_socks(&mut io, &mut out, &[client, server, Sock(0)]);
}

/// Nothing of the sends is left: the backend takes as many operations
/// as it was built for.
fn room_for_every_operation<IO: Io>(io: &mut IO, out: &mut Completions) {
    let ops = net_limits().ops_max();
    for k in 0..ops {
        io.must_submit(Token(500 + u64::from(k)), Op::Timeout { nanos: 1 });
    }
    let mut left = ops;
    drain_until(io, out, |_, c| {
        assert!((500..500 + u64::from(ops)).contains(&c.token.0), "{c:?}");
        left -= 1;
        left == 0
    });
}

/// A close, or a cancel and then a close, between the two reports: the
/// last still comes, once.
fn the_last_report_comes_whatever_ends_it<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
    cancel: bool,
) {
    let mut io = make(net_limits());
    let mut out = Completions::new(8);
    let client = Sock(1);
    let server = connect_one(&mut io, &mut out, addr, Sock(0), client);
    io.buf(BufId(0))[..64].copy_from_slice(&pattern(0));
    io.must_submit(Token(100), send_zc(client, 0, 0));
    let (mut first, mut done) = (None, None);
    drain_until(&mut io, &mut out, |_, c| {
        assert_eq!(c.token.0, 100, "{c:?}");
        done = note(&mut first, c);
        true
    });
    if cancel && done.is_none() {
        io.must_submit(Token(101), Op::Cancel { target: Token(100) });
    }
    io.must_submit(Token(102), Op::Close { sock: client });
    let mut closed = false;
    drain_until(&mut io, &mut out, |_, c| {
        match c.token.0 {
            100 => {
                assert!(done.is_none(), "a second last report: {c:?}");
                done = note(&mut first, c);
            }
            // A cancel that finds nothing to end says so; either is right.
            101 => {}
            102 => closed = true,
            other => fail(format_args!("token {other}: {c:?}")),
        }
        closed && done.is_some()
    });
    room_for_every_operation(&mut io, &mut out);
    close_socks(&mut io, &mut out, &[server, Sock(0)]);
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    the_next_send_follows_the_first_report(make, addr);
    the_last_report_comes_whatever_ends_it(make, addr, false);
    the_last_report_comes_whatever_ends_it(make, addr, true);
}
