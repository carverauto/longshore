// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared armed-accept contract checks.

use super::{close_socks, drain_one, drain_until, net_limits, send_all};
use crate::io::{
    BufId, Completion, Completions, Io, Limits, Op, Sock, Token, ECANCELED, EMFILE, ENFILE,
};
use std::net::SocketAddr;

fn several_accepts_on_one_armed_accept<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let mut io = make(Limits {
        sockets_max: 8,
        accept_range_start: 1,
        accept_range_len: 4,
        ..net_limits()
    });
    let mut out = Completions::new(8);
    let bound = io.listen(Sock(0), addr).expect("listen");
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for k in 0..3u32 {
        io.submit(
            Token(10 + u64::from(k)),
            Op::Connect {
                sock: Sock(5 + k),
                addr: bound,
            },
        )
        .unwrap();
    }
    let mut accepted = Vec::new();
    let mut conns = 0usize;
    let mut accept_armed = true;
    while accepted.len() < 3 || conns < 3 {
        if !accept_armed && accepted.len() < 3 {
            io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
                .unwrap();
            accept_armed = true;
        }
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == 1 {
                accept_armed = c.more;
                if c.result >= 0 {
                    assert!(c.buf.is_none() && (1..5).contains(&c.result));
                    accepted.push(Sock(c.result as u32));
                }
            } else {
                assert_eq!(c.result, 0);
                conns += 1;
            }
            (accepted.len() == 3 && conns == 3) || !accept_armed
        });
    }
    accepted.sort_by_key(|s| s.0);
    accepted.dedup();
    assert_eq!(accepted.len(), 3);
    if accept_armed {
        io.submit(Token(20), Op::Cancel { target: Token(1) })
            .unwrap();
        let (mut cancel_ok, mut accept_end) = (false, false);
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == 20 {
                assert_eq!(c.result, 0);
                cancel_ok = true;
            } else if c.token.0 == 1 {
                assert!(!c.more && c.buf.is_none() && c.result == -ECANCELED);
                accept_end = true;
            }
            cancel_ok && accept_end
        });
    }
    close_socks(
        &mut io,
        &mut out,
        &[
            Sock(0),
            Sock(1),
            Sock(2),
            Sock(3),
            Sock(5),
            Sock(6),
            Sock(7),
        ],
    );
}

fn accept_range_full_and_armed_again<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let mut io = make(Limits {
        sockets_max: 7,
        accept_range_start: 1,
        accept_range_len: 2,
        ..net_limits()
    });
    let mut out = Completions::new(8);
    let bound = io.listen(Sock(0), addr).expect("listen");
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for k in 0..2u32 {
        io.submit(
            Token(10 + u64::from(k)),
            Op::Connect {
                sock: Sock(3 + k),
                addr: bound,
            },
        )
        .unwrap();
    }
    let mut accepted = Vec::new();
    let mut conns = 0;
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 1 {
            assert!(c.more && (1..3).contains(&c.result));
            accepted.push(Sock(c.result as u32));
        } else {
            assert_eq!(c.result, 0);
            conns += 1;
        }
        accepted.len() == 2 && conns == 2
    });

    // 3rd client arrives when range 1..3 is full: dropped and ArmedAccept ends with -ENFILE/-EMFILE.
    io.submit(
        Token(12),
        Op::Connect {
            sock: Sock(5),
            addr: bound,
        },
    )
    .unwrap();
    let (mut accept_err, mut third_done) = (false, false);
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 1 {
            assert!(!c.more && c.buf.is_none());
            assert!(c.result == -ENFILE || c.result == -EMFILE, "{c:?}");
            accept_err = true;
        } else if c.token.0 == 12 {
            third_done = true;
        }
        accept_err && third_done
    });
    close_socks(&mut io, &mut out, &[Sock(5), accepted[0]]);
    io.submit(Token(2), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(13),
        Op::Connect {
            sock: Sock(6),
            addr: bound,
        },
    )
    .unwrap();
    let mut reused = None;
    let mut accept2_armed = true;
    let mut fourth_ok = false;
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 2 {
            assert!(c.result == accepted[0].0 as i32);
            accept2_armed = c.more;
            reused = Some(Sock(c.result as u32));
        } else if c.token.0 == 13 {
            assert_eq!(c.result, 0);
            fourth_ok = true;
        }
        reused.is_some() && fourth_ok
    });
    if accept2_armed {
        io.submit(Token(21), Op::Cancel { target: Token(2) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 2 && !c.more);
    }
    let mut tok = 100;
    send_all(&mut io, &mut out, Sock(6), BufId(0), b"rearmed", &mut tok);
    io.submit(
        Token(tok),
        Op::Recv {
            sock: reused.unwrap(),
            buf: BufId(1),
            off: 0,
            len: 32,
        },
    )
    .unwrap();
    assert!(drain_one(&mut io, &mut out, tok).result > 0);
    close_socks(
        &mut io,
        &mut out,
        &[
            Sock(0),
            accepted[1],
            reused.unwrap(),
            Sock(3),
            Sock(4),
            Sock(6),
        ],
    );
}

fn accept_range_len_zero_returns_enfile<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let mut io = make(Limits {
        sockets_max: 4,
        accept_range_start: 0,
        accept_range_len: 0,
        ..net_limits()
    });
    let mut out = Completions::new(8);
    let bound = io.listen(Sock(0), addr).expect("listen");

    // With no incoming client, ArmedAccept with accept_range_len == 0
    // completes immediately with -ENFILE on tick(false) without blocking.
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.tick(false, &mut out).unwrap();
    assert_eq!(out.pop(), Some(Completion::new(Token(1), -ENFILE)));
    assert!(out.is_empty());

    // Even with a client connecting, ArmedAccept with accept_range_len == 0
    // completes at submit time with -ENFILE without accepting or resetting the client.
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr: bound,
        },
    )
    .unwrap();
    io.submit(Token(3), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.tick(false, &mut out).unwrap();
    let mut got_accept = None;
    let mut connect_done = false;
    while let Some(c) = out.pop() {
        match c.token.0 {
            3 => got_accept = Some(c),
            2 => {
                assert_eq!(c, Completion::new(Token(2), 0));
                connect_done = true;
            }
            other => panic!("unexpected completion token {other}"),
        }
    }
    assert_eq!(got_accept, Some(Completion::new(Token(3), -ENFILE)));
    if !connect_done {
        io.submit(Token(4), Op::Cancel { target: Token(2) })
            .unwrap();
        let mut cancel_done = false;
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == 2 {
                assert!(c.result == 0 || c.result == -ECANCELED, "{c:?}");
                connect_done = true;
            } else if c.token.0 == 4 {
                cancel_done = true;
            }
            connect_done && cancel_done
        });
    }
    close_socks(&mut io, &mut out, &[Sock(0), Sock(1)]);
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    several_accepts_on_one_armed_accept(make, addr);
    accept_range_full_and_armed_again(make, addr);
    accept_range_len_zero_returns_enfile(make, addr);
}
