// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared armed-receive and buffer-ring contract checks.

use super::{
    cancel_and_wait, close_socks, connect_one, drain_until, echo_armed, net_limits, send_rest,
};
use crate::io::must::{must, must_len, Must};
use crate::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ENOBUFS};
use std::net::SocketAddr;

fn armed_recv_bytes_and_enobufs_and_eof<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let limits = net_limits().with_ring_buffers(1).with_ring_buffer_size(128);
    let ring = limits.ring(0);
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let srv = connect_one(&mut io, &mut out, addr, Sock(0), Sock(1));
    let mut tok = 60;
    let (got1, armed50) = echo_armed(
        &mut io,
        &mut out,
        (Sock(1), srv),
        BufId(0),
        b"first",
        50,
        &mut tok,
    );
    assert_eq!(got1, b"first");
    if armed50 {
        cancel_and_wait(&mut io, &mut out, tok, 50);
        tok += 1;
    }

    let (held, mut held_bytes, armed51) = hold_ring_buffer(&mut io, &mut out, srv, ring, &mut tok);
    enobufs_while_held(&mut io, &mut out, srv, armed51, &mut tok);
    assert_eq!(&io.buf(held)[..held_bytes.len()], held_bytes.as_slice());
    io.give_back(held);
    let armed52 = recv_after_give_back(&mut io, &mut out, srv, ring, &mut held_bytes);
    assert_eq!(held_bytes, b"holdnext");
    if armed52 {
        cancel_and_wait(&mut io, &mut out, tok, 52);
        tok += 1;
    }
    eof_on_peer_close(&mut io, &mut out, srv, tok);
    tok += 1;
    close_socks(&mut io, &mut out, &[srv, Sock(0)]);
    ring_buffer_reused(&mut io, &mut out, addr, &mut tok);
}

/// Arm receive 51 and send "hold" until it lands in the one ring buffer, which
/// is kept. Returns the buffer, its bytes, and whether 51 is still armed.
fn hold_ring_buffer<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    srv: Sock,
    ring: BufId,
    tok: &mut u64,
) -> (BufId, Vec<u8>, bool) {
    io.must_submit(Token(51), Op::ArmedRecv { sock: srv });
    let mut armed51 = true;
    // Wait for both Send and first ArmedRecv completion (hold the buffer!).
    let mut held = None;
    let mut held_bytes = Vec::new();
    let mut send_off = 0usize;
    let hold_msg = b"hold";
    while held.is_none() || send_off < hold_msg.len() {
        let cur_tok = send_rest(io, (Sock(1), BufId(0)), hold_msg, send_off, tok);
        let mut send_done = cur_tok.is_none();
        drain_until(io, out, |io, c| {
            if c.token.0 == 51 && held.is_none() {
                assert!(c.result > 0, "hold recv: {c:?}");
                armed51 = c.more;
                let b = must(c.buf, "buffer of a receive that returned bytes");
                assert_eq!(b, ring);
                held_bytes.extend_from_slice(&io.buf(b)[..must_len(c.result)]);
                held = Some(b);
                send_done
            } else if Some(c.token.0) == cur_tok {
                assert!(c.result > 0, "hold send: {c:?}");
                send_off += must_len(c.result);
                send_done = true;
                held.is_some()
            } else {
                false
            }
        });
    }
    (must(held, "held buffer"), held_bytes, armed51)
}

/// Send "next" while the ring buffer is held: receive 51 ends with -ENOBUFS.
fn enobufs_while_held<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    srv: Sock,
    mut armed51: bool,
    tok: &mut u64,
) {
    let next_msg = b"next";
    let mut next_off = 0usize;
    let mut nobufs = false;
    while !nobufs || next_off < next_msg.len() {
        if !armed51 {
            io.must_submit(Token(51), Op::ArmedRecv { sock: srv });
            armed51 = true;
        }
        let cur_tok = send_rest(io, (Sock(1), BufId(0)), next_msg, next_off, tok);
        let mut send_done = cur_tok.is_none();
        drain_until(io, out, |_, c| {
            if c.token.0 == 51 {
                assert_eq!((c.result, c.more, c.buf), (-ENOBUFS, false, None));
                armed51 = false;
                nobufs = true;
            } else if Some(c.token.0) == cur_tok {
                assert!(c.result > 0, "next send: {c:?}");
                next_off += must_len(c.result);
                send_done = true;
            }
            send_done && nobufs
        });
    }
}

/// With the ring buffer given back, receive 52 collects "next" into `bytes`.
/// Returns whether 52 is still armed.
fn recv_after_give_back<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    srv: Sock,
    ring: BufId,
    bytes: &mut Vec<u8>,
) -> bool {
    io.must_submit(Token(52), Op::ArmedRecv { sock: srv });
    let mut armed52 = true;
    while bytes.len() < 8 {
        if !armed52 {
            io.must_submit(Token(52), Op::ArmedRecv { sock: srv });
            armed52 = true;
        }
        let mut got_chunk = false;
        drain_until(io, out, |io, c| {
            if c.token.0 == 52 {
                armed52 = c.more;
                if c.result > 0 {
                    let b = must(c.buf, "buffer of a receive that returned bytes");
                    assert_eq!(b, ring);
                    bytes.extend_from_slice(&io.buf(b)[..must_len(c.result)]);
                    io.give_back(b);
                    got_chunk = true;
                }
            }
            got_chunk || !armed52
        });
    }
    armed52
}

/// Peer closes -> 0, more: false, buf: None, no buffer leaked.
fn eof_on_peer_close<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock, tok: u64) {
    io.must_submit(Token(53), Op::ArmedRecv { sock: srv });
    io.must_submit(Token(tok), Op::Close { sock: Sock(1) });
    let (mut closed_cli, mut got_eof) = (false, false);
    drain_until(io, out, |_, c| {
        if c.token.0 == tok {
            assert_eq!(c.result, 0);
            closed_cli = true;
        } else if c.token.0 == 53 {
            assert_eq!((c.result, c.more, c.buf), (0, false, None));
            got_eof = true;
        }
        closed_cli && got_eof
    });
}

/// A new connection still obtains the one ring buffer.
fn ring_buffer_reused<IO: Io>(io: &mut IO, out: &mut Completions, addr: SocketAddr, tok: &mut u64) {
    let srv2 = connect_one(io, out, addr, Sock(0), Sock(1));
    let (got_ok, armed54) = echo_armed(io, out, (Sock(1), srv2), BufId(0), b"ok", 54, tok);
    assert_eq!(got_ok, b"ok");
    if armed54 {
        cancel_and_wait(io, out, *tok, 54);
    }
    close_socks(io, out, &[Sock(0), Sock(1), srv2]);
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    armed_recv_bytes_and_enobufs_and_eof(make, addr);
}
