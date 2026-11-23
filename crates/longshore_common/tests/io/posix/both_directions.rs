// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A Send and a Recv waiting on the same `PosixIo` socket at once, each
//! completing on its own readiness.

use super::{drain_n, pair, LIMITS};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{BufId, Completion, Completions, Io, Op, Sock, Token};

/// How long a Send must stay pending before the test treats the socket as full.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Ticks without blocking until a completion arrives or `window` passes.
fn completion_within(
    io: &mut PosixIo,
    out: &mut Completions,
    window: std::time::Duration,
) -> Option<Completion> {
    let start = std::time::Instant::now();
    loop {
        io.tick(false, out).unwrap();
        if let Some(c) = out.pop() {
            return Some(c);
        }
        if start.elapsed() >= window {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn send(sock: Sock, buf: u32, len: u32) -> Op {
    Op::Send {
        sock,
        buf: BufId(buf),
        off: 0,
        len,
        zero_copy: false,
    }
}

fn recv(sock: Sock, buf: u32, len: u32) -> Op {
    Op::Recv {
        sock,
        buf: BufId(buf),
        off: 0,
        len,
    }
}

/// The next unused token, advancing `tok`.
fn next(tok: &mut u64) -> u64 {
    let t = *tok;
    *tok += 1;
    t
}

/// Fills Sock(2)'s send buffer until a Send stalls waiting for writability;
/// the stalled Send's token.
///
/// One tick without a completion is not enough: on macOS a loopback socket
/// turns writable again within a fraction of a millisecond, several times,
/// while the kernel moves data into the peer and auto-tunes both buffers.
/// A Send counts as stalled only when it stays pending for `SETTLE`.
fn fill_until_a_send_stalls(io: &mut PosixIo, out: &mut Completions, tok: &mut u64) -> u64 {
    io.buf(BufId(0)).fill(0xab);
    let mut pending_send_tok = None;
    for _ in 0..256 {
        let t = next(tok);
        io.submit(Token(t), send(Sock(2), 0, 65536)).unwrap();
        if let Some(c) = completion_within(io, out, SETTLE) {
            assert_eq!(c.token.0, t);
            assert!(c.result > 0, "send failed: {}", c.result);
        } else {
            pending_send_tok = Some(t);
            break;
        }
    }
    pending_send_tok.expect("expected socket send buffer to fill")
}

/// Waits until the Send and the Recv are both pending; the token of the
/// Send that is.
///
/// Under load the kernel can drain a "full" loopback socket later than
/// `SETTLE`, so a Send that completes anyway is replaced by another until one
/// stays pending: the test then starts from a Send that is really waiting,
/// however long the drain took.
fn until_both_pending(
    io: &mut PosixIo,
    out: &mut Completions,
    tok: &mut u64,
    mut send_tok: u64,
) -> u64 {
    let mut both_pending = false;
    for _ in 0..256 {
        match completion_within(io, out, SETTLE) {
            None => {
                both_pending = true;
                break;
            }
            Some(c) if c.token.0 == send_tok => {
                assert!(c.result > 0, "send failed: {}", c.result);
                send_tok = next(tok);
                io.submit(Token(send_tok), send(Sock(2), 0, 65536)).unwrap();
            }
            Some(c) => panic!(
                "unexpected completion: token {} result {}",
                c.token.0, c.result
            ),
        }
    }
    assert!(
        both_pending,
        "a Send and a Recv must both be pending on Sock(2)"
    );
    send_tok
}

/// Ticks until the Recv completes with "hello"; whether the stalled Send
/// completed along the way.
fn recv_hello(io: &mut PosixIo, out: &mut Completions, recv_tok: u64, send_tok: u64) -> bool {
    let mut saw_recv = false;
    let mut saw_send = false;
    while !saw_recv {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == recv_tok {
                assert_eq!(c.result, 5);
                assert_eq!(&io.buf(BufId(1))[..5], b"hello");
                saw_recv = true;
            } else if c.token.0 == send_tok {
                saw_send = true;
            }
        }
    }
    saw_send
}

/// Drains Sock(1) until the blocked Send on Sock(2) completes.
fn drain_until_send_completes(
    io: &mut PosixIo,
    out: &mut Completions,
    tok: &mut u64,
    send_tok: u64,
    mut saw_send: bool,
) {
    while !saw_send {
        io.submit(Token(next(tok)), recv(Sock(1), 3, 65536))
            .unwrap();
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == send_tok {
                assert!(c.result > 0, "stalled send failed: {}", c.result);
                saw_send = true;
            }
        }
    }
}

#[test]
fn both_directions_wait_independently_on_one_socket() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(8);
    pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    let mut tok = 10u64;
    let stalled = fill_until_a_send_stalls(&mut io, &mut out, &mut tok);

    // Now submit Recv on the same Sock(2) while Send is still waiting.
    let recv_tok = next(&mut tok);
    io.submit(Token(recv_tok), recv(Sock(2), 1, 64)).unwrap();
    let send_tok = until_both_pending(&mut io, &mut out, &mut tok, stalled);

    // Send 5 bytes from peer Sock(1) -> Sock(2); Recv must complete although a
    // Send on the same socket is pending.
    io.buf(BufId(2))[..5].copy_from_slice(b"hello");
    io.submit(Token(next(&mut tok)), send(Sock(1), 2, 5))
        .unwrap();
    // The stalled Send may complete here too, without Sock(1) reading: the
    // segment carrying the reply can also carry an acknowledgement that frees
    // room in Sock(2)'s send buffer (about 1 run in 40 on macOS loopback).
    let saw_send = recv_hello(&mut io, &mut out, recv_tok, send_tok);
    drain_until_send_completes(&mut io, &mut out, &mut tok, send_tok, saw_send);

    for (k, sock) in [Sock(0), Sock(1), Sock(2)].into_iter().enumerate() {
        io.submit(Token(900 + k as u64), Op::Close { sock })
            .unwrap();
    }
    let _: Vec<(u64, i32)> = drain_n(&mut io, &mut out, 3);
}
