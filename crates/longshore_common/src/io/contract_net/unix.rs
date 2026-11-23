// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared socket-options and Unix-domain stream socket contract checks.

use super::{close_socks, drain_one, drain_until, echo_armed, net_limits};
use crate::io::must::Must;
use crate::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

fn put_bytes(io: &mut impl Io, buf: BufId, bytes: &[u8]) -> u32 {
    io.buf(buf)[..bytes.len()].copy_from_slice(bytes);
    bytes.len() as u32
}

fn socket_options_echo<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let mut io = make(net_limits());
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    let bound = io.must_listen(Sock(0), addr);
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr: bound,
        },
    )
    .unwrap();
    let mut accepted = None;
    let mut accept_armed = true;
    let mut connected = false;
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 1 {
            assert!(c.result >= 0);
            accept_armed = c.more;
            accepted = Some(Sock(u32::try_from(c.result).unwrap()));
        } else if c.token.0 == 2 {
            assert_eq!(c.result, 0);
            connected = true;
        }
        accepted.is_some() && connected
    });
    let srv = accepted.unwrap();
    if accept_armed {
        io.submit(Token(20), Op::Cancel { target: Token(1) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 1 && !c.more);
    }
    let mut tok = 30u64;
    let payload: Vec<u8> = (0..1024usize).map(|i| (i * 7 + 3) as u8).collect();
    let (got, recv_armed) = echo_armed(
        &mut io,
        &mut out,
        (Sock(1), srv),
        BufId(0),
        &payload,
        3,
        &mut tok,
    );
    assert_eq!(got, payload);
    if recv_armed {
        io.submit(Token(tok), Op::Cancel { target: Token(3) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 3 && !c.more);
    }
    close_socks(&mut io, &mut out, &[Sock(0), Sock(1), srv]);
}

fn unix_no_listener_and_bad_paths_leave_slot_free<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
    unix_path: &str,
) {
    let mut io = make(net_limits());
    let mut out = Completions::new(8);

    // Non-listening Unix path completes negatively and leaves Sock(1) free.
    let mut with_nul = unix_path.as_bytes().to_vec();
    with_nul.push(0);
    let len = put_bytes(&mut io, BufId(0), &with_nul);
    io.submit(
        Token(1),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    )
    .unwrap();
    let c = drain_one(&mut io, &mut out, 1);
    assert!(c.result < 0 && !c.more && c.buf.is_none(), "{c:?}");

    // Bad paths (missing NUL, interior NUL, overlong) fail negatively and leave Sock(1) free.
    let no_nul = put_bytes(&mut io, BufId(0), unix_path.as_bytes());
    io.submit(
        Token(2),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: no_nul,
        },
    )
    .unwrap();
    assert!(drain_one(&mut io, &mut out, 2).result < 0);

    let mut interior = b"/tmp/bad\0path".to_vec();
    interior.push(0);
    let ilen = put_bytes(&mut io, BufId(0), &interior);
    io.submit(
        Token(3),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: ilen,
        },
    )
    .unwrap();
    assert!(drain_one(&mut io, &mut out, 3).result < 0);

    let mut long_path = vec![b'a'; 109];
    long_path[0] = b'/';
    long_path.push(0);
    let llen = put_bytes(&mut io, BufId(0), &long_path);
    io.submit(
        Token(4),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: llen,
        },
    )
    .unwrap();
    assert!(drain_one(&mut io, &mut out, 4).result < 0);

    // Now reuse Sock(1) for a valid TCP connect.
    let bound = io.must_listen(Sock(0), addr);
    io.submit(
        Token(5),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(6),
        Op::Connect {
            sock: Sock(1),
            addr: bound,
        },
    )
    .unwrap();
    let (mut a, mut b) = (false, false);
    drain_until(&mut io, &mut out, |_, done| {
        if done.token.0 == 5 {
            assert_eq!(done.result, 0);
            a = true;
        } else if done.token.0 == 6 {
            assert_eq!(done.result, 0);
            b = true;
        }
        a && b
    });
    close_socks(&mut io, &mut out, &[Sock(0), Sock(1), Sock(2)]);
}

fn unix_stream_echo<IO: Io>(make: &mut impl FnMut(Limits) -> IO, unix_path: &str) {
    let mut io = make(net_limits());
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    io.must_listen_unix(Sock(0), unix_path);
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    let mut with_nul = unix_path.as_bytes().to_vec();
    with_nul.push(0);
    let len = put_bytes(&mut io, BufId(0), &with_nul);
    io.submit(
        Token(2),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    )
    .unwrap();
    // Overwrite pool buffer immediately to prove submission-time path copy.
    io.buf(BufId(0)).fill(0xaa);

    let mut accepted = None;
    let mut accept_armed = true;
    let mut connected = false;
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == 1 {
            assert!(c.result >= 0);
            accept_armed = c.more;
            accepted = Some(Sock(u32::try_from(c.result).unwrap()));
        } else if c.token.0 == 2 {
            assert_eq!(c.result, 0, "ConnectUnix failed: {c:?}");
            connected = true;
        }
        accepted.is_some() && connected
    });
    let srv = accepted.unwrap();
    if accept_armed {
        io.submit(Token(10), Op::Cancel { target: Token(1) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 1 && !c.more);
    }

    let payload: Vec<u8> = (0..16 * 1024usize).map(|i| (i * 13 + 5) as u8).collect();
    let mut tok = 20u64;
    let (got, recv_armed) = echo_armed(
        &mut io,
        &mut out,
        (Sock(1), srv),
        BufId(0),
        &payload,
        3,
        &mut tok,
    );
    assert_eq!(got, payload);
    if recv_armed {
        io.submit(Token(tok), Op::Cancel { target: Token(3) })
            .unwrap();
        drain_until(&mut io, &mut out, |_, c| c.token.0 == 3 && !c.more);
    }
    close_socks(&mut io, &mut out, &[Sock(0), Sock(1), srv]);
}

pub(super) fn run_all<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
    unix_path: &str,
) {
    socket_options_echo(make, addr);
    unix_no_listener_and_bad_paths_leave_slot_free(make, addr, unix_path);
    unix_stream_echo(make, unix_path);
}
