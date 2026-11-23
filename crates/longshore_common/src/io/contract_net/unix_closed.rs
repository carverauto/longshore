// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared Unix-domain closed-peer-before-accept regression checks.

use super::{cancel_until_end, close_socks, drain_one, drain_until, echo_armed, net_limits};
use crate::io::must::{must, must_sock, Must};
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
    io.must_submit(
        Token(tok),
        Op::ConnectUnix {
            sock,
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    );
    io.must_submit(Token(tok + 1), Op::Timeout { nanos: 100_000_000 });
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
        io.must_submit(
            Token(tok + 2),
            Op::Cancel {
                target: Token(tok + 1),
            },
        );
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
    io.must_submit(Token(tok + 3), Op::Close { sock });
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

    let accept_armed = armed_accept_of_closed_peer(&mut io, &mut out, &with_nul);
    live_peer_after_closed(&mut io, &mut out, &with_nul, accept_armed);
    plain_accept_after_closed(&mut io, &mut out, &with_nul);
    close_socks(&mut io, &mut out, &[Sock(0)]);
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_file(unix_path).ok();
}

/// Submit a Unix connect of `sock` to the path `with_nul` with token `tok`.
fn submit_connect_unix<IO: Io>(io: &mut IO, tok: u64, sock: Sock, with_nul: &[u8]) {
    let path_len = put_bytes(io, BufId(0), with_nul);
    io.must_submit(
        Token(tok),
        Op::ConnectUnix {
            sock,
            path_buf: BufId(0),
            path_off: 0,
            path_len,
        },
    );
}

/// A peer that connected and closed before armed accept 1 is accepted, and its
/// socket reads EOF. Returns whether accept 1 is still armed.
fn armed_accept_of_closed_peer<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    with_nul: &[u8],
) -> bool {
    let was_connected = connect_unix_then_close(io, out, with_nul, Sock(1), 10);
    io.must_submit(Token(1), Op::ArmedAccept { listener: Sock(0) });
    if !was_connected {
        return true;
    }
    let a = drain_one(io, out, 1);
    assert!(a.result >= 0 && a.more && a.buf.is_none(), "{a:?}");
    let closed_srv = must_sock(a.result);
    io.must_submit(Token(20), Op::ArmedRecv { sock: closed_srv });
    let eof = drain_one(io, out, 20);
    assert_eq!((eof.result, eof.more, eof.buf), (0, false, None));
    close_socks(io, out, &[closed_srv]);
    a.more
}

/// A live peer is then accepted by accept 1 and echoes.
fn live_peer_after_closed<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    with_nul: &[u8],
    mut accept_armed: bool,
) {
    submit_connect_unix(io, 30, Sock(1), with_nul);
    let (mut live_srv, mut live_conn) = (None, false);
    drain_until(io, out, |_, c| {
        if c.token.0 == 1 {
            assert!(c.result >= 0 && c.buf.is_none(), "{c:?}");
            accept_armed = c.more;
            live_srv = Some(must_sock(c.result));
        } else if c.token.0 == 30 {
            assert_eq!(c.result, 0);
            live_conn = true;
        }
        live_srv.is_some() && live_conn
    });
    let live_srv = must(live_srv, "live server socket");
    if accept_armed {
        cancel_until_end(io, out, 31, 1);
    }
    let mut tok = 60u64;
    let (got, recv_armed) = echo_armed(
        io,
        out,
        (Sock(1), live_srv),
        BufId(0),
        b"after-closed-peer",
        3,
        &mut tok,
    );
    assert_eq!(got, b"after-closed-peer");
    if recv_armed {
        cancel_until_end(io, out, tok, 3);
    }
    close_socks(io, out, &[Sock(1), live_srv]);
}

/// A plain accept 50 takes a closed peer (which reads EOF) or, if the peer never
/// connected, a fresh one.
fn plain_accept_after_closed<IO: Io>(io: &mut IO, out: &mut Completions, with_nul: &[u8]) {
    let was_connected2 = connect_unix_then_close(io, out, with_nul, Sock(1), 40);
    io.must_submit(
        Token(50),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    );
    if was_connected2 {
        let a = drain_one(io, out, 50);
        assert_eq!((a.result, a.more, a.buf), (0, false, None));
        io.must_submit(
            Token(51),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(1),
                off: 0,
                len: 32,
            },
        );
        assert_eq!(drain_one(io, out, 51).result, 0);
        close_socks(io, out, &[Sock(2)]);
    } else {
        submit_connect_unix(io, 52, Sock(1), with_nul);
        let (mut a_ok, mut c_ok) = (false, false);
        drain_until(io, out, |_, c| {
            if c.token.0 == 50 {
                assert_eq!(c.result, 0);
                a_ok = true;
            } else if c.token.0 == 52 {
                assert_eq!(c.result, 0);
                c_ok = true;
            }
            a_ok && c_ok
        });
        close_socks(io, out, &[Sock(1), Sock(2)]);
    }
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, unix_path: &str) {
    unix_closed_peer_before_accept(make, unix_path);
}
