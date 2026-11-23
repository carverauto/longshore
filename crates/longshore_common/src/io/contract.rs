// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared [`Io`] contract checks executable against any backend (`SimIo`, `PosixIo`, `UringIo`).

use crate::io::must::{fail, must_len, Must};
use crate::io::scenario::echo;
use crate::io::{BufId, Busy, Completions, Io, Limits, Op, Sock, Token, EAGAIN, ECANCELED};
use std::net::SocketAddr;

const LISTENER: Sock = Sock(0);
const CLIENT: Sock = Sock(1);
const SERVER: Sock = Sock(2);

fn drain_n<IO: Io>(io: &mut IO, out: &mut Completions, expected: usize) -> Vec<(u64, i32)> {
    let mut got = Vec::with_capacity(expected);
    while got.len() < expected {
        io.must_tick(true, out);
        while let Some(c) = out.pop() {
            got.push((c.token.0, c.result));
        }
    }
    got
}

fn connect_pair<IO: Io>(io: &mut IO, out: &mut Completions, addr: SocketAddr) -> SocketAddr {
    let bound = io.must_listen(LISTENER, addr);
    io.must_submit(Token(1), accept_op(LISTENER, SERVER));
    io.must_submit(
        Token(2),
        Op::Connect {
            sock: CLIENT,
            addr: bound,
        },
    );
    for (t, r) in drain_n(io, out, 2) {
        assert!(r >= 0, "pair setup op {t} failed: {r}");
    }
    bound
}

fn accept_op(listener: Sock, sock: Sock) -> Op {
    Op::Accept { listener, sock }
}

fn recv_now_op(sock: Sock, buf: BufId, len: u32) -> Op {
    Op::RecvNow {
        sock,
        buf,
        off: 0,
        len,
    }
}

fn recv_op(sock: Sock, buf: BufId, len: u32) -> Op {
    Op::Recv {
        sock,
        buf,
        off: 0,
        len,
    }
}

fn send_op(sock: Sock, buf: BufId, off: u32, len: u32) -> Op {
    Op::Send {
        sock,
        buf,
        off,
        len,
        zero_copy: false,
    }
}

fn io_limits() -> Limits {
    Limits::BASE
        .with_sockets_max(4)
        .with_buffers(2)
        .with_buffer_size(1024)
        .with_ops_max(8)
}

fn small_limits() -> Limits {
    Limits::BASE
        .with_sockets_max(1)
        .with_buffers(1)
        .with_buffer_size(64)
        .with_ops_max(4)
}

fn close_sockets<IO: Io>(io: &mut IO, out: &mut Completions, socks: &[Sock]) {
    for (k, &sock) in socks.iter().enumerate() {
        io.must_submit(Token(900 + k as u64), Op::Close { sock });
    }
    let _: Vec<(u64, i32)> = drain_n(io, out, socks.len());
}

/// Echo scenario with `zero_copy: false` and `zero_copy: true`, plus single-completion check.
pub fn one_completion_and_echo<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = io_limits()
        .with_buffers(3)
        .with_buffer_size(4096)
        .with_ops_max(4);
    for zc in [false, true] {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        io.must_submit(Token(42), Op::Timeout { nanos: 0 });
        assert_eq!(drain_n(&mut io, &mut out, 1), vec![(42, 0)]);
        io.must_tick(false, &mut out);
        assert!(out.is_empty(), "extra completion delivered");

        let stats = echo(&mut io, addr, 64 * 1024, 4096, zc, &mut out)
            .unwrap_or_else(|e| fail(format_args!("echo(zc={zc}) failed: {e:?}")));
        assert!(stats.sends > 0 && stats.recvs > 0 && stats.ticks > 0);
    }
}

/// `submit` refuses with `Busy` when `ops_max` are in flight and delivers no extra completion.
pub fn capacity_busy<IO: Io>(make: &mut impl FnMut(Limits) -> IO) {
    let mut io = make(small_limits().with_ops_max(3));
    let mut out = Completions::new(8);
    for t in 1..=3 {
        io.must_submit(Token(t), Op::Timeout { nanos: 5_000_000 });
    }
    assert_eq!(
        io.submit(Token(4), Op::Timeout { nanos: 0 }),
        Err(Busy),
        "fourth submit beyond ops_max must return Busy"
    );
    let mut got = drain_n(&mut io, &mut out, 3);
    got.sort_by_key(|x| x.0);
    assert_eq!(got, vec![(1, 0), (2, 0), (3, 0)]);
    io.must_tick(false, &mut out);
    assert!(out.is_empty(), "refused Busy op must not complete");

    io.must_submit(Token(5), Op::Timeout { nanos: 0 });
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(5, 0)]);
}

/// All three cancellation contract cases: waiting target, missing target, reused token.
pub fn cancel_cases<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = io_limits();

    // 1. Cancelled while waiting.
    {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        let _: SocketAddr = connect_pair(&mut io, &mut out, addr);
        io.must_submit(Token(10), recv_op(SERVER, BufId(0), 1024));
        io.must_tick(false, &mut out);
        assert!(out.is_empty());

        io.must_submit(Token(11), Op::Cancel { target: Token(10) });
        let mut res = drain_n(&mut io, &mut out, 2);
        res.sort_by_key(|x| x.0);
        assert_eq!(res, vec![(10, -ECANCELED), (11, 0)]);
        close_sockets(&mut io, &mut out, &[CLIENT, SERVER, LISTENER]);
    }

    // 2. Nothing to cancel.
    {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        io.must_submit(Token(20), Op::Cancel { target: Token(999) });
        let res = drain_n(&mut io, &mut out, 1);
        assert_eq!(res[0].0, 20);
        assert!(res[0].1 < 0, "cancel of missing token must be negative");
    }

    // 3. Token used again after cancel is submitted.
    {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        io.must_submit(Token(30), Op::Cancel { target: Token(77) });
        io.must_submit(Token(77), Op::Timeout { nanos: 1_000_000 });
        let mut res = drain_n(&mut io, &mut out, 2);
        res.sort_by_key(|x| x.0);
        assert_eq!(res[0].0, 30);
        assert!(res[0].1 < 0, "cancel submitted first must miss new token");
        assert_eq!(res[1], (77, 0));
    }
}

/// All three close contract cases: recv in flight, accept in flight, empty slot.
pub fn close_cases<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = io_limits();

    // 1. Receive in flight on closed socket completes without waiting on peer.
    {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        let _: SocketAddr = connect_pair(&mut io, &mut out, addr);
        io.must_submit(Token(10), recv_op(SERVER, BufId(0), 1024));
        io.must_tick(false, &mut out);
        io.must_submit(Token(11), Op::Close { sock: SERVER });
        let mut res = drain_n(&mut io, &mut out, 2);
        res.sort_by_key(|x| x.0);
        assert_eq!(res[0].0, 10);
        assert!(res[0].1 < 0, "recv on closed socket must fail: {res:?}");
        assert_eq!(res[1].0, 11);
        close_sockets(&mut io, &mut out, &[CLIENT, LISTENER]);
    }

    // 2. Accept in flight on closed listener completes without waiting.
    {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        let _: SocketAddr = io.must_listen(LISTENER, addr);
        io.must_submit(Token(20), accept_op(LISTENER, SERVER));
        io.must_tick(false, &mut out);
        io.must_submit(Token(21), Op::Close { sock: LISTENER });
        let mut res = drain_n(&mut io, &mut out, 2);
        res.sort_by_key(|x| x.0);
        assert_eq!(res[0].0, 20);
        assert!(res[0].1 < 0, "accept on closed listener must fail: {res:?}");
        assert_eq!(res[1].0, 21);
    }

    // 3. Empty slot close completes once.
    {
        let mut io = make(limits);
        let mut out = Completions::new(8);
        io.must_submit(Token(30), Op::Close { sock: Sock(0) });
        let res = drain_n(&mut io, &mut out, 1);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].0, 30);
        io.must_tick(false, &mut out);
        assert!(out.is_empty());
    }
}

/// Timers complete with 0 after their deadline.
pub fn timers<IO: Io>(make: &mut impl FnMut(Limits) -> IO) {
    let mut io = make(small_limits());
    let mut out = Completions::new(8);
    let t0 = io.now_ns();
    io.must_submit(Token(1), Op::Timeout { nanos: 60_000_000 });
    io.must_submit(Token(2), Op::Timeout { nanos: 10_000_000 });
    io.must_submit(Token(3), Op::Timeout { nanos: 35_000_000 });
    let mut res = drain_n(&mut io, &mut out, 3);
    let elapsed = io.now_ns().saturating_sub(t0);
    assert!(
        elapsed >= 58_000_000,
        "longest 60ms timer elapsed only {elapsed} ns"
    );
    res.sort_by_key(|x| x.0);
    assert_eq!(res, vec![(1, 0), (2, 0), (3, 0)]);
}

/// Pre-signalled wake completes `AwaitWake`.
pub fn wake_ready<IO: Io>(make: &mut impl FnMut(Limits) -> IO) {
    let mut io = make(small_limits());
    let mut out = Completions::new(8);
    let waker = io.must_waker();
    waker.wake();
    io.must_submit(Token(1), Op::AwaitWake);
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(1, 0)]);
}

/// `RecvNow` returns `-EAGAIN` when empty, bytes when ready, `0` on EOF, and
/// sending to a closed peer fails negatively without `SIGPIPE`.
pub fn recv_now_and_closed_peer<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let mut io = make(io_limits());
    let mut out = Completions::new(8);
    let _: SocketAddr = connect_pair(&mut io, &mut out, addr);

    io.must_submit(Token(10), recv_now_op(SERVER, BufId(0), 64));
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(10, -EAGAIN)]);

    let msg = b"contract-stream-bytes";
    io.buf(BufId(1))[..msg.len()].copy_from_slice(msg);
    let mut sent = 0usize;
    let mut tok = 20u64;
    while sent < msg.len() {
        let rem = (msg.len() - sent) as u32;
        io.must_submit(Token(tok), send_op(CLIENT, BufId(1), sent as u32, rem));
        let res = drain_n(&mut io, &mut out, 1)[0].1;
        assert!(res > 0, "send failed: {res}");
        sent += must_len(res);
        tok += 1;
    }

    let mut got = Vec::new();
    while got.len() < msg.len() {
        io.must_submit(Token(tok), recv_op(SERVER, BufId(0), 64));
        let n = drain_n(&mut io, &mut out, 1)[0].1;
        assert!(n > 0, "recv failed: {n}");
        got.extend_from_slice(&io.buf(BufId(0))[..must_len(n)]);
        tok += 1;
    }
    assert_eq!(got.as_slice(), msg);

    // Close client, expect EOF (0) on server.
    close_sockets(&mut io, &mut out, &[CLIENT]);
    io.must_submit(Token(tok), recv_op(SERVER, BufId(0), 64));
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(tok, 0)]);
    tok += 1;

    // Sending on a connection whose peer closed must fail negatively without SIGPIPE.
    let mut failed = false;
    for _ in 0..16 {
        io.buf(BufId(1))[..16].fill(0x5a);
        io.must_submit(Token(tok), send_op(SERVER, BufId(1), 0, 16));
        let r = drain_n(&mut io, &mut out, 1)[0].1;
        tok += 1;
        if r < 0 {
            failed = true;
            break;
        }
        io.must_submit(Token(tok), Op::Timeout { nanos: 1_000_000 });
        let _: Vec<(u64, i32)> = drain_n(&mut io, &mut out, 1);
        tok += 1;
    }
    assert!(failed, "send to closed peer should fail negatively");
    close_sockets(&mut io, &mut out, &[SERVER, LISTENER]);
}

/// `Cancel` targeting `Close` fails negatively; `Close` still closes the socket.
pub fn cancel_never_cancels_close<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    for shift in 0..3u64 {
        for tick_recv in [false, true] {
            let mut io = make(io_limits());
            let mut out = Completions::new(8);
            let _: SocketAddr = connect_pair(&mut io, &mut out, addr);
            for k in 0..shift {
                io.must_submit(Token(100 + k), Op::Timeout { nanos: 0 });
                assert_eq!(drain_n(&mut io, &mut out, 1), vec![(100 + k, 0)]);
            }
            io.must_submit(Token(10), recv_op(SERVER, BufId(0), 64));
            if tick_recv {
                io.must_tick(false, &mut out);
                assert!(out.is_empty());
            }
            io.must_submit(Token(11), Op::Close { sock: SERVER });
            io.must_submit(Token(12), Op::Cancel { target: Token(11) });
            let mut res = drain_n(&mut io, &mut out, 3);
            res.sort_by_key(|x| x.0);
            assert!(res[0].0 == 10 && res[0].1 < 0, "recv failed: {res:?}");
            assert_eq!(res[1], (11, 0), "Close must complete with 0: {res:?}");
            assert!(res[2].0 == 12 && res[2].1 < 0, "Cancel(Close): {res:?}");
            let mut eof = -EAGAIN;
            for step in 0..64u64 {
                io.must_submit(Token(13 + step * 2), recv_now_op(CLIENT, BufId(1), 64));
                let r = drain_n(&mut io, &mut out, 1)[0].1;
                if r != -EAGAIN {
                    eof = r;
                    break;
                }
                io.must_submit(Token(14 + step * 2), Op::Timeout { nanos: 500_000 });
                let _: Vec<(u64, i32)> = drain_n(&mut io, &mut out, 1);
            }
            assert_eq!(eof, 0, "peer EOF");
            close_sockets(&mut io, &mut out, &[CLIENT, LISTENER]);
        }
    }
}

/// Run the full shared `Io` contract suite against `IO`.
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    one_completion_and_echo(make, addr);
    capacity_busy(make);
    cancel_cases(make, addr);
    close_cases(make, addr);
    cancel_never_cancels_close(make, addr);
    timers(make);
    wake_ready(make);
    recv_now_and_closed_peer(make, addr);
}
