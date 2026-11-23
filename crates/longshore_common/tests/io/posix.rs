// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Readiness, wake, starvation, and reset tests for `PosixIo` (`kqueue` / `epoll`).

#![cfg(unix)]

use longshore_common::io::posix::errno::linux;
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

const LIMITS: Limits = Limits {
    sockets_max: 12,
    buffers: 4,
    buffer_size: 65536,
    ops_max: 16,
    ..Limits::BASE
};

fn v4() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn drain_n(io: &mut PosixIo, out: &mut Completions, want: usize) -> Vec<(u64, i32)> {
    let mut got = Vec::with_capacity(want);
    while got.len() < want {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            got.push((c.token.0, c.result));
        }
    }
    got
}

fn pair(io: &mut PosixIo, out: &mut Completions, listener: Sock, server: Sock, client: Sock) {
    let bound = io.listen(listener, v4()).unwrap();
    io.submit(
        Token(1),
        Op::Accept {
            listener,
            sock: server,
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: client,
            addr: bound,
        },
    )
    .unwrap();
    for (t, r) in drain_n(io, out, 2) {
        assert_eq!(r, 0, "pair setup op {t} failed: {r}");
    }
}

/// How long a Send must stay pending before the test treats the socket as full.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Ticks without blocking until a completion arrives or `window` passes.
fn completion_within(
    io: &mut PosixIo,
    out: &mut Completions,
    window: std::time::Duration,
) -> Option<longshore_common::io::Completion> {
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

#[test]
fn both_directions_wait_independently_on_one_socket() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(8);
    pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    // Fill Sock(2)'s send buffer until Send stalls waiting for writability.
    // One tick without a completion is not enough: on macOS a loopback socket
    // turns writable again within a fraction of a millisecond, several times,
    // while the kernel moves data into the peer and auto-tunes both buffers.
    // A Send counts as stalled only when it stays pending for `SETTLE`.
    io.buf(BufId(0)).fill(0xab);
    let mut tok = 10u64;
    let mut pending_send_tok = None;
    for _ in 0..256 {
        io.submit(
            Token(tok),
            Op::Send {
                sock: Sock(2),
                buf: BufId(0),
                off: 0,
                len: 65536,
                zero_copy: false,
            },
        )
        .unwrap();
        if let Some(c) = completion_within(&mut io, &mut out, SETTLE) {
            assert_eq!(c.token.0, tok);
            assert!(c.result > 0, "send failed: {}", c.result);
            tok += 1;
        } else {
            pending_send_tok = Some(tok);
            tok += 1;
            break;
        }
    }
    let mut send_tok = pending_send_tok.expect("expected socket send buffer to fill");

    // Now submit Recv on the same Sock(2) while Send is still waiting.
    let recv_tok = tok;
    tok += 1;
    io.submit(
        Token(recv_tok),
        Op::Recv {
            sock: Sock(2),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    // Both must now be pending. Under load the kernel can drain a "full"
    // loopback socket later than `SETTLE`, so a Send that completes anyway is
    // replaced by another until one stays pending: the test then starts from
    // a Send that is really waiting, however long the drain took.
    let mut both_pending = false;
    for _ in 0..256 {
        match completion_within(&mut io, &mut out, SETTLE) {
            None => {
                both_pending = true;
                break;
            }
            Some(c) if c.token.0 == send_tok => {
                assert!(c.result > 0, "send failed: {}", c.result);
                send_tok = tok;
                tok += 1;
                io.submit(
                    Token(send_tok),
                    Op::Send {
                        sock: Sock(2),
                        buf: BufId(0),
                        off: 0,
                        len: 65536,
                        zero_copy: false,
                    },
                )
                .unwrap();
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

    // Send 5 bytes from peer Sock(1) -> Sock(2); Recv must complete although a
    // Send on the same socket is pending.
    io.buf(BufId(2))[..5].copy_from_slice(b"hello");
    io.submit(
        Token(tok),
        Op::Send {
            sock: Sock(1),
            buf: BufId(2),
            off: 0,
            len: 5,
            zero_copy: false,
        },
    )
    .unwrap();
    tok += 1;

    let mut saw_recv = false;
    let mut saw_send = false;
    while !saw_recv {
        io.tick(true, &mut out).unwrap();
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
    // The stalled Send may complete here too, without Sock(1) reading: the
    // segment carrying the reply can also carry an acknowledgement that frees
    // room in Sock(2)'s send buffer (about 1 run in 40 on macOS loopback).

    // Drain Sock(1) until the blocked Send on Sock(2) completes.
    while !saw_send {
        io.submit(
            Token(tok),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(3),
                off: 0,
                len: 65536,
            },
        )
        .unwrap();
        tok += 1;
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == send_tok {
                assert!(c.result > 0, "stalled send failed: {}", c.result);
                saw_send = true;
            }
        }
    }

    for (k, sock) in [Sock(0), Sock(1), Sock(2)].into_iter().enumerate() {
        io.submit(Token(900 + k as u64), Op::Close { sock })
            .unwrap();
    }
    let _: Vec<(u64, i32)> = drain_n(&mut io, &mut out, 3);
}

#[test]
fn cross_thread_wake_and_wake_after_drop() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(4);
    let waker = io.waker().unwrap();
    let waker2 = io.waker().unwrap();

    io.submit(Token(1), Op::AwaitWake).unwrap();
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(15));
        waker.wake();
    });
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().map(|c| (c.token.0, c.result)), Some((1, 0)));
    h.join().unwrap();

    // Waker owns its duplicated fd and must stay safe after PosixIo drops.
    drop(io);
    waker2.wake();
}

#[test]
fn full_completions_buffer_does_not_lose_readiness() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut big = Completions::new(16);
    pair(&mut io, &mut big, Sock(0), Sock(1), Sock(2));
    pair(&mut io, &mut big, Sock(3), Sock(4), Sock(5));
    pair(&mut io, &mut big, Sock(6), Sock(7), Sock(8));
    pair(&mut io, &mut big, Sock(9), Sock(10), Sock(11));

    let senders = [Sock(1), Sock(4), Sock(7), Sock(10)];
    let receivers = [Sock(2), Sock(5), Sock(8), Sock(11)];

    for (i, &s) in senders.iter().enumerate() {
        io.buf(BufId(0))[0] = i as u8;
        io.submit(
            Token(10 + i as u64),
            Op::Send {
                sock: s,
                buf: BufId(0),
                off: 0,
                len: 1,
                zero_copy: false,
            },
        )
        .unwrap();
    }
    let _: Vec<(u64, i32)> = drain_n(&mut io, &mut big, 4);

    // Wait briefly so all 4 loopback segments are in the kernel rx buffers, then
    // arm 4 Recvs and drain one per tick with capacity 1.
    std::thread::sleep(std::time::Duration::from_millis(5));
    for (i, &r) in receivers.iter().enumerate() {
        io.submit(
            Token(100 + i as u64),
            Op::Recv {
                sock: r,
                buf: BufId(i as u32),
                off: 0,
                len: 1,
            },
        )
        .unwrap();
    }

    let mut small = Completions::new(1);
    let mut got = Vec::new();
    for _ in 0..4 {
        io.tick(true, &mut small).unwrap();
        let c = small.pop().expect("one completion per tick");
        assert!(small.is_empty());
        assert_eq!(c.result, 1);
        got.push(c.token.0);
    }
    got.sort();
    assert_eq!(got, vec![100, 101, 102, 103]);
}

#[test]
fn continuous_submitting_caller_does_not_starve_ready_socket() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(1);
    let mut setup_out = Completions::new(4);
    pair(&mut io, &mut setup_out, Sock(0), Sock(1), Sock(2));

    io.submit(
        Token(1),
        Op::Recv {
            sock: Sock(2),
            buf: BufId(0),
            off: 0,
            len: 8,
        },
    )
    .unwrap();
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());

    io.buf(BufId(1))[..4].copy_from_slice(b"data");
    io.submit(
        Token(2),
        Op::Send {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    // Drain the Send first.
    let mut sent = false;
    while !sent {
        io.tick(true, &mut out).unwrap();
        if let Some(c) = out.pop() {
            if c.token.0 == 2 {
                sent = true;
            } else if c.token.0 == 1 {
                return;
            }
        }
    }

    // Now submit an immediately-ready Timeout every turn with a 1-slot buffer.
    let mut got_recv = false;
    for k in 10..40 {
        io.submit(Token(k), Op::Timeout { nanos: 0 }).unwrap();
        io.tick(true, &mut out).unwrap();
        let c = out.pop().unwrap();
        if c.token.0 == 1 {
            assert_eq!(c.result, 4);
            got_recv = true;
            break;
        }
    }
    assert!(got_recv, "ready socket recv starved by immediate timeouts");
}

#[test]
fn reset_peer_yields_linux_econnreset() {
    let listener = TcpListener::bind(v4()).unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || listener.accept().unwrap().0);
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(4);

    io.submit(
        Token(1),
        Op::Connect {
            sock: Sock(0),
            addr,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(1, 0)]);
    let peer = h.join().unwrap();

    // Send unread bytes to peer, wait for delivery, then drop peer so TCP sends RST.
    io.buf(BufId(0))[..4].copy_from_slice(b"data");
    io.submit(
        Token(2),
        Op::Send {
            sock: Sock(0),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(2, 4)]);

    io.submit(
        Token(3),
        Op::Recv {
            sock: Sock(0),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.tick(false, &mut out).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(10));
    drop(peer);
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(3, -linux::ECONNRESET)]);

    // Follow-up send on reset socket yields -ECONNRESET or -EPIPE without SIGPIPE.
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(0),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    let r = drain_n(&mut io, &mut out, 1)[0].1;
    assert!(
        r == -linux::ECONNRESET || r == -linux::EPIPE,
        "unexpected closed errno {r}"
    );
}
