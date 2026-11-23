// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared armed-receive cancellation, close-with-armed, idle-connection, and small-buffer checks.

use super::{close_socks, connect_one, drain_one, drain_until, net_limits};
use crate::io::must::Must;
use crate::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ECANCELED};
use std::net::SocketAddr;

fn cancel_and_close_armed_recv<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = Limits {
        ring_buffers: 1,
        ring_buffer_size: 128,
        ..net_limits()
    };
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let srv = connect_one(&mut io, &mut out, addr, Sock(0), Sock(1));
    io.submit(Token(10), Op::ArmedRecv { sock: srv }).unwrap();
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());
    io.submit(Token(11), Op::Cancel { target: Token(10) })
        .unwrap();
    let (mut r_done, mut c_done) = (false, false);
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 10 {
            assert_eq!((c.result, c.more, c.buf), (-ECANCELED, false, None));
            r_done = true;
        } else if c.token.0 == 11 {
            assert_eq!(c.result, 0);
            c_done = true;
        }
        r_done && c_done
    });
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());

    // Re-arm and close socket with receive armed; peer sees EOF (0).
    io.submit(Token(12), Op::ArmedRecv { sock: srv }).unwrap();
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());
    io.submit(Token(13), Op::Close { sock: srv }).unwrap();
    let (mut r2, mut cl) = (false, false);
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 12 {
            assert!(!c.more && c.buf.is_none() && c.result < 0, "{c:?}");
            r2 = true;
        } else if c.token.0 == 13 {
            assert_eq!(c.result, 0);
            cl = true;
        }
        r2 && cl
    });
    io.submit(
        Token(14),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 32,
        },
    )
    .unwrap();
    let peer_eof = drain_one(&mut io, &mut out, 14);
    assert_eq!(
        (peer_eof.result, peer_eof.more, peer_eof.buf),
        (0, false, None)
    );
    close_socks(&mut io, &mut out, &[Sock(0), Sock(1)]);
}

fn tens_of_idle_connections_and_small_out<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    const N: u32 = 20;
    let limits = Limits {
        sockets_max: 2 + N * 2,
        accept_range_start: 1,
        accept_range_len: N,
        buffers: 2,
        buffer_size: 256,
        ring_buffers: 1,
        ring_buffer_size: 128,
        ops_max: 64,
        ..Limits::BASE
    };
    let mut io = make(limits);
    let mut out = Completions::new(1);
    let bound = io.must_listen(Sock(0), addr);
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for k in 0..N {
        io.submit(
            Token(100 + u64::from(k)),
            Op::Connect {
                sock: Sock(1 + N + k),
                addr: bound,
            },
        )
        .unwrap();
    }
    let mut accepted = Vec::new();
    let mut conns = 0u32;
    let mut accept_armed = true;
    while accepted.len() < N as usize || conns < N {
        if !accept_armed && accepted.len() < N as usize {
            io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
                .unwrap();
            accept_armed = true;
        }
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == 1 {
                accept_armed = c.more;
                if c.result >= 0 {
                    accepted.push(Sock(u32::try_from(c.result).unwrap()));
                }
            } else {
                assert_eq!(c.result, 0);
                conns += 1;
            }
            (accepted.len() == N as usize && conns == N) || !accept_armed
        });
    }
    if accept_armed {
        io.submit(Token(2), Op::Cancel { target: Token(1) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 1 && !c.more);
    }

    for (k, &s) in accepted.iter().enumerate() {
        io.submit(Token(200 + k as u64), Op::ArmedRecv { sock: s })
            .unwrap();
    }
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty(), "idle connections must hold 0 ring buffers");

    // Multi-turn Completions::new(1) burst across connections (accept order may vary).
    let mut tok = 500u64;
    let cli_socks = [Sock(1 + N), Sock(2 + N), Sock(N * 2)];
    let wants: [&[u8]; 3] = [b"burst-a", b"burst-b", b"last"];
    let mut got = vec![Vec::new(); N as usize];
    let mut sent = [0usize; 3];
    for idx in 0..3 {
        while sent[idx] < wants[idx].len() || !got.iter().any(|g| g == wants[idx]) {
            let mut cur_tok = None;
            if sent[idx] < wants[idx].len() {
                let rem = &wants[idx][sent[idx]..];
                io.buf(BufId(0))[..rem.len()].copy_from_slice(rem);
                let t = tok;
                tok += 1;
                cur_tok = Some(t);
                io.submit(
                    Token(t),
                    Op::Send {
                        sock: cli_socks[idx],
                        buf: BufId(0),
                        off: 0,
                        len: rem.len() as u32,
                        zero_copy: false,
                    },
                )
                .unwrap();
            }
            let mut send_done = cur_tok.is_none();
            drain_until(&mut io, &mut out, |io, c| {
                if (200..200 + u64::from(N)).contains(&c.token.0) {
                    let pos = (c.token.0 - 200) as usize;
                    if c.result > 0 {
                        let b = c.buf.unwrap();
                        got[pos]
                            .extend_from_slice(&io.buf(b)[..usize::try_from(c.result).unwrap()]);
                        io.give_back(b);
                    }
                    if !c.more && got[pos].len() < wants[idx].len() {
                        io.submit(
                            Token(c.token.0),
                            Op::ArmedRecv {
                                sock: accepted[pos],
                            },
                        )
                        .unwrap();
                    }
                } else if Some(c.token.0) == cur_tok {
                    assert!(c.result > 0, "burst send: {c:?}");
                    sent[idx] += usize::try_from(c.result).unwrap();
                    send_done = true;
                }
                send_done && (sent[idx] < wants[idx].len() || got.iter().any(|g| g == wants[idx]))
            });
        }
    }

    let mut all: Vec<Sock> = vec![Sock(0)];
    all.extend(accepted);
    for k in 0..N {
        all.push(Sock(1 + N + k));
    }
    close_socks(&mut io, &mut out, &all);
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    cancel_and_close_armed_recv(make, addr);
    tens_of_idle_connections_and_small_out(make, addr);
}
