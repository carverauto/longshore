// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared networking contract suite for armed accept/recv, buffer ring, socket options, and Unix sockets.

use crate::io::must::{fail, must, must_len, must_sock, Must};
use crate::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

mod accept;
mod recv;
mod recv_idle;
mod unix;
mod unix_closed;

pub(crate) fn net_limits() -> Limits {
    Limits {
        sockets_max: 8,
        accept_range_start: 2,
        accept_range_len: 4,
        buffers: 2,
        buffer_size: 512,
        ring_buffers: 2,
        ring_buffer_size: 128,
        ops_max: 16,
        ..Limits::BASE
    }
}

pub(crate) fn drain_until<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    mut done: impl FnMut(&mut IO, Completion) -> bool,
) {
    for _ in 0..50_000 {
        io.must_tick(true, out);
        while let Some(c) = out.pop() {
            if done(io, c) {
                return;
            }
        }
    }
    fail(format_args!("timed out waiting for completions"));
}

pub(crate) fn drain_one<IO: Io>(io: &mut IO, out: &mut Completions, tok: u64) -> Completion {
    let mut got = None;
    drain_until(io, out, |_, c| {
        if c.token.0 == tok {
            got = Some(c);
            return true;
        }
        false
    });
    must(got, "completion")
}

pub(crate) fn echo_armed<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    (tx, rx): (Sock, Sock),
    tx_buf: BufId,
    payload: &[u8],
    rx_tok: u64,
    tx_tok: &mut u64,
) -> (Vec<u8>, bool) {
    io.must_submit(Token(rx_tok), Op::ArmedRecv { sock: rx });
    let mut armed = true;
    let mut sent = 0usize;
    let mut got = Vec::with_capacity(payload.len());
    let mut send_in_flight = false;
    let mut cur_tok = 0u64;
    let buf_cap = io.buf(tx_buf).len();

    while got.len() < payload.len() {
        if !send_in_flight && sent < payload.len() {
            let chunk = (payload.len() - sent).min(buf_cap);
            io.buf(tx_buf)[..chunk].copy_from_slice(&payload[sent..sent + chunk]);
            cur_tok = *tx_tok;
            *tx_tok += 1;
            io.must_submit(
                Token(cur_tok),
                Op::Send {
                    sock: tx,
                    buf: tx_buf,
                    off: 0,
                    len: chunk as u32,
                    zero_copy: false,
                },
            );
            send_in_flight = true;
        }
        drain_until(io, out, |io, c| {
            if c.token.0 == rx_tok {
                armed = c.more;
                if c.result > 0 {
                    let b = must(c.buf, "buffer of a receive that returned bytes");
                    got.extend_from_slice(&io.buf(b)[..must_len(c.result)]);
                    io.give_back(b);
                }
                if !armed && got.len() < payload.len() {
                    io.must_submit(Token(rx_tok), Op::ArmedRecv { sock: rx });
                    armed = true;
                }
                true
            } else if send_in_flight && c.token.0 == cur_tok {
                assert!(c.result > 0 && !c.more && c.buf.is_none(), "send: {c:?}");
                sent += must_len(c.result);
                send_in_flight = false;
                true
            } else {
                false
            }
        });
    }
    while send_in_flight {
        let c = drain_one(io, out, cur_tok);
        assert!(c.result > 0);
        send_in_flight = false;
    }
    (got, armed)
}

pub(crate) fn send_all<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    sock: Sock,
    buf: BufId,
    data: &[u8],
    tok: &mut u64,
) {
    let mut sent = 0usize;
    while sent < data.len() {
        let chunk = (data.len() - sent).min(io.buf(buf).len());
        io.buf(buf)[..chunk].copy_from_slice(&data[sent..sent + chunk]);
        let t = *tok;
        *tok += 1;
        io.must_submit(
            Token(t),
            Op::Send {
                sock,
                buf,
                off: 0,
                len: chunk as u32,
                zero_copy: false,
            },
        );
        let c = drain_one(io, out, t);
        assert!(c.result > 0 && !c.more && c.buf.is_none(), "send: {c:?}");
        sent += must_len(c.result);
    }
}

pub(crate) fn close_socks<IO: Io>(io: &mut IO, out: &mut Completions, socks: &[Sock]) {
    let mut pending = socks.len();
    for (k, &sock) in socks.iter().enumerate() {
        io.must_submit(Token(9_000 + k as u64), Op::Close { sock });
    }
    if pending == 0 {
        return;
    }
    drain_until(io, out, |_, c| {
        if (9_000..9_000 + socks.len() as u64).contains(&c.token.0) {
            assert!(!c.more && c.buf.is_none());
            pending -= 1;
        }
        pending == 0
    });
}

pub(crate) fn connect_one<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    addr: SocketAddr,
    listener: Sock,
    client: Sock,
) -> Sock {
    let bound = io.must_listen(listener, addr);
    io.must_submit(Token(1), Op::ArmedAccept { listener });
    io.must_submit(
        Token(2),
        Op::Connect {
            sock: client,
            addr: bound,
        },
    );
    let mut accepted = None;
    let mut accept_armed = true;
    let mut connected = false;
    drain_until(io, out, |_, c| {
        if c.token.0 == 1 {
            assert!(c.result >= 0 && c.buf.is_none(), "accept: {c:?}");
            accepted = Some(must_sock(c.result));
            accept_armed = c.more;
        } else if c.token.0 == 2 {
            assert_eq!(c.result, 0);
            connected = true;
        }
        accepted.is_some() && connected
    });
    if accept_armed {
        io.must_submit(Token(3), Op::Cancel { target: Token(1) });
        let (mut c_done, mut a_done) = (false, false);
        drain_until(io, out, |_, c| {
            if c.token.0 == 3 {
                c_done = true;
            } else if c.token.0 == 1 && !c.more {
                a_done = true;
            }
            c_done && a_done
        });
    }
    must(accepted, "accepted socket")
}

/// Run the full shared server-networking contract suite against `IO`.
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr, unix_path: &str) {
    accept::run_all(make, addr);
    recv::run_all(make, addr);
    recv_idle::run_all(make, addr);
    unix::run_all(make, addr, unix_path);
    unix_closed::run_all(make, unix_path);
}
