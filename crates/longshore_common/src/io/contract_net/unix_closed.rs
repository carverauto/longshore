// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared Unix-domain closed-peer-before-accept regression checks.

use super::{close_socks, drain_one, drain_until, echo_armed, net_limits};
use crate::io::must::Must;
use crate::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ECANCELED};

fn put_bytes(io: &mut impl Io, buf: BufId, bytes: &[u8]) -> u32 {
    io.buf(buf)[..bytes.len()].copy_from_slice(bytes);
    bytes.len() as u32
}

fn connect_unix_then_close<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    with_nul: &[u8],
    sock: Sock,
    tok: u64,
) -> bool {
    let len = put_bytes(io, BufId(0), with_nul);
    io.submit(
        Token(tok),
        Op::ConnectUnix {
            sock,
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    )
    .unwrap();
    io.submit(Token(tok + 1), Op::Timeout { nanos: 100_000_000 })
        .unwrap();
    let (mut connected, mut timed_out) = (false, false);
    drain_until(io, out, |_, c| {
        if c.token.0 == tok {
            assert_eq!(c.result, 0, "ConnectUnix failed: {c:?}");
            connected = true;
        } else if c.token.0 == tok + 1 {
            assert_eq!(c.result, 0);
            timed_out = true;
        }
        connected || timed_out
    });
    if connected {
        io.submit(
            Token(tok + 2),
            Op::Cancel {
                target: Token(tok + 1),
            },
        )
        .unwrap();
        let (mut c_ok, mut t_end) = (false, false);
        drain_until(io, out, |_, c| {
            if c.token.0 == tok + 2 {
                c_ok = true;
            } else if c.token.0 == tok + 1 {
                t_end = true;
            }
            c_ok && t_end
        });
    }
    io.submit(Token(tok + 3), Op::Close { sock }).unwrap();
    if connected {
        assert_eq!(drain_one(io, out, tok + 3).result, 0);
    } else {
        let (mut c_end, mut closed) = (false, false);
        drain_until(io, out, |_, c| {
            if c.token.0 == tok {
                assert_eq!(c.result, -ECANCELED);
                c_end = true;
            } else if c.token.0 == tok + 3 {
                assert_eq!(c.result, 0);
                closed = true;
            }
            c_end && closed
        });
    }
    connected
}

fn unix_closed_peer_before_accept<IO: Io>(make: &mut impl FnMut(Limits) -> IO, unix_path: &str) {
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_file(unix_path).ok();
    let mut io = make(net_limits());
    let mut out = Completions::new(8);
    io.must_listen_unix(Sock(0), unix_path);
    let mut with_nul = unix_path.as_bytes().to_vec();
    with_nul.push(0);

    let was_connected = connect_unix_then_close(&mut io, &mut out, &with_nul, Sock(1), 10);
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    let mut accept_armed = true;
    if was_connected {
        let a = drain_one(&mut io, &mut out, 1);
        assert!(a.result >= 0 && a.more && a.buf.is_none(), "{a:?}");
        accept_armed = a.more;
        let closed_srv = Sock(u32::try_from(a.result).unwrap());
        io.submit(Token(20), Op::ArmedRecv { sock: closed_srv })
            .unwrap();
        let eof = drain_one(&mut io, &mut out, 20);
        assert_eq!((eof.result, eof.more, eof.buf), (0, false, None));
        close_socks(&mut io, &mut out, &[closed_srv]);
    }

    let len = put_bytes(&mut io, BufId(0), &with_nul);
    io.submit(
        Token(30),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    )
    .unwrap();
    let (mut live_srv, mut live_conn) = (None, false);
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 1 {
            assert!(c.result >= 0 && c.buf.is_none(), "{c:?}");
            accept_armed = c.more;
            live_srv = Some(Sock(u32::try_from(c.result).unwrap()));
        } else if c.token.0 == 30 {
            assert_eq!(c.result, 0);
            live_conn = true;
        }
        live_srv.is_some() && live_conn
    });
    let live_srv = live_srv.unwrap();
    if accept_armed {
        io.submit(Token(31), Op::Cancel { target: Token(1) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 1 && !c.more);
    }
    let mut tok = 60u64;
    let (got, recv_armed) = echo_armed(
        &mut io,
        &mut out,
        (Sock(1), live_srv),
        BufId(0),
        b"after-closed-peer",
        3,
        &mut tok,
    );
    assert_eq!(got, b"after-closed-peer");
    if recv_armed {
        io.submit(Token(tok), Op::Cancel { target: Token(3) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 3 && !c.more);
    }
    close_socks(&mut io, &mut out, &[Sock(1), live_srv]);

    let was_connected2 = connect_unix_then_close(&mut io, &mut out, &with_nul, Sock(1), 40);
    io.submit(
        Token(50),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    if was_connected2 {
        let a = drain_one(&mut io, &mut out, 50);
        assert_eq!((a.result, a.more, a.buf), (0, false, None));
        io.submit(
            Token(51),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(1),
                off: 0,
                len: 32,
            },
        )
        .unwrap();
        assert_eq!(drain_one(&mut io, &mut out, 51).result, 0);
        close_socks(&mut io, &mut out, &[Sock(2)]);
    } else {
        let len2 = put_bytes(&mut io, BufId(0), &with_nul);
        io.submit(
            Token(52),
            Op::ConnectUnix {
                sock: Sock(1),
                path_buf: BufId(0),
                path_off: 0,
                path_len: len2,
            },
        )
        .unwrap();
        let (mut a_ok, mut c_ok) = (false, false);
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == 50 {
                assert_eq!(c.result, 0);
                a_ok = true;
            } else if c.token.0 == 52 {
                assert_eq!(c.result, 0);
                c_ok = true;
            }
            a_ok && c_ok
        });
        close_socks(&mut io, &mut out, &[Sock(1), Sock(2)]);
    }
    close_socks(&mut io, &mut out, &[Sock(0)]);
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_file(unix_path).ok();
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, unix_path: &str) {
    unix_closed_peer_before_accept(make, unix_path);
}
