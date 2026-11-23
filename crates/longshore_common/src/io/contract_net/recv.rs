// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared armed-receive and buffer-ring contract checks.

use super::{close_socks, connect_one, drain_until, echo_armed, net_limits};
use crate::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ENOBUFS};
use std::net::SocketAddr;

fn armed_recv_bytes_and_enobufs_and_eof<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let limits = Limits {
        ring_buffers: 1,
        ring_buffer_size: 128,
        ..net_limits()
    };
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let srv = connect_one(&mut io, &mut out, addr, Sock(0), Sock(1));
    let mut tok = 60;
    let (got1, mut armed50) = echo_armed(
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
        io.submit(Token(tok), Op::Cancel { target: Token(50) })
            .unwrap();
        let (mut c_ok, mut r_end) = (false, false);
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == tok {
                c_ok = true;
            } else if c.token.0 == 50 && !c.more {
                r_end = true;
            }
            c_ok && r_end
        });
        tok += 1;
        armed50 = false;
    }
    let _: bool = armed50;

    io.submit(Token(51), Op::ArmedRecv { sock: srv }).unwrap();
    let mut armed51 = true;
    // Send "hold" and wait for both Send and first ArmedRecv completion (hold the buffer!).
    let mut held = None;
    let mut held_bytes = Vec::new();
    let mut send_off = 0usize;
    let hold_msg = b"hold";
    while held.is_none() || send_off < hold_msg.len() {
        let mut cur_tok = None;
        if send_off < hold_msg.len() {
            let rem = hold_msg.len() - send_off;
            io.buf(BufId(0))[..rem].copy_from_slice(&hold_msg[send_off..]);
            let t = tok;
            tok += 1;
            cur_tok = Some(t);
            io.submit(
                Token(t),
                Op::Send {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: rem as u32,
                    zero_copy: false,
                },
            )
            .unwrap();
        }
        let mut send_done = cur_tok.is_none();
        drain_until(&mut io, &mut out, |io, c| {
            if c.token.0 == 51 && held.is_none() {
                assert!(c.result > 0, "hold recv: {c:?}");
                armed51 = c.more;
                let b = c.buf.unwrap();
                assert_eq!(b, limits.ring(0));
                held_bytes.extend_from_slice(&io.buf(b)[..usize::try_from(c.result).unwrap()]);
                held = Some(b);
                send_done
            } else if Some(c.token.0) == cur_tok {
                assert!(c.result > 0, "hold send: {c:?}");
                send_off += usize::try_from(c.result).unwrap();
                send_done = true;
                held.is_some()
            } else {
                false
            }
        });
    }
    let held = held.unwrap();

    // Now send "next" while `held` is NOT returned -> ArmedRecv ends with -ENOBUFS.
    let next_msg = b"next";
    let mut next_off = 0usize;
    let mut nobufs = false;
    while !nobufs || next_off < next_msg.len() {
        if !armed51 {
            io.submit(Token(51), Op::ArmedRecv { sock: srv }).unwrap();
            armed51 = true;
        }
        let mut cur_tok = None;
        if next_off < next_msg.len() {
            let rem = next_msg.len() - next_off;
            io.buf(BufId(0))[..rem].copy_from_slice(&next_msg[next_off..]);
            let t = tok;
            tok += 1;
            cur_tok = Some(t);
            io.submit(
                Token(t),
                Op::Send {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: rem as u32,
                    zero_copy: false,
                },
            )
            .unwrap();
        }
        let mut send_done = cur_tok.is_none();
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == 51 {
                assert_eq!((c.result, c.more, c.buf), (-ENOBUFS, false, None));
                armed51 = false;
                nobufs = true;
            } else if Some(c.token.0) == cur_tok {
                assert!(c.result > 0, "next send: {c:?}");
                next_off += usize::try_from(c.result).unwrap();
                send_done = true;
            }
            send_done && nobufs
        });
    }
    assert_eq!(&io.buf(held)[..held_bytes.len()], held_bytes.as_slice());
    io.give_back(held);

    io.submit(Token(52), Op::ArmedRecv { sock: srv }).unwrap();
    let mut armed52 = true;
    while held_bytes.len() < 8 {
        if !armed52 {
            io.submit(Token(52), Op::ArmedRecv { sock: srv }).unwrap();
            armed52 = true;
        }
        let mut got_chunk = false;
        drain_until(&mut io, &mut out, |io, c| {
            if c.token.0 == 52 {
                armed52 = c.more;
                if c.result > 0 {
                    let b = c.buf.unwrap();
                    assert_eq!(b, limits.ring(0));
                    held_bytes.extend_from_slice(&io.buf(b)[..usize::try_from(c.result).unwrap()]);
                    io.give_back(b);
                    got_chunk = true;
                }
            }
            got_chunk || !armed52
        });
    }
    assert_eq!(held_bytes, b"holdnext");
    if armed52 {
        io.submit(Token(tok), Op::Cancel { target: Token(52) })
            .unwrap();
        let (mut c_ok, mut r_end) = (false, false);
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == tok {
                c_ok = true;
            } else if c.token.0 == 52 && !c.more {
                r_end = true;
            }
            c_ok && r_end
        });
        tok += 1;
    }

    // Peer closes -> 0, more: false, buf: None, no buffer leaked.
    io.submit(Token(53), Op::ArmedRecv { sock: srv }).unwrap();
    io.submit(Token(tok), Op::Close { sock: Sock(1) }).unwrap();
    let (mut closed_cli, mut got_eof) = (false, false);
    drain_until(&mut io, &mut out, |_, c| {
        if c.token.0 == tok {
            assert_eq!(c.result, 0);
            closed_cli = true;
        } else if c.token.0 == 53 {
            assert_eq!((c.result, c.more, c.buf), (0, false, None));
            got_eof = true;
        }
        closed_cli && got_eof
    });
    tok += 1;
    close_socks(&mut io, &mut out, &[srv, Sock(0)]);

    // New connection still obtains the 1 ring buffer.
    let srv2 = connect_one(&mut io, &mut out, addr, Sock(0), Sock(1));
    let (got_ok, armed54) = echo_armed(
        &mut io,
        &mut out,
        (Sock(1), srv2),
        BufId(0),
        b"ok",
        54,
        &mut tok,
    );
    assert_eq!(got_ok, b"ok");
    if armed54 {
        io.submit(Token(tok), Op::Cancel { target: Token(54) })
            .unwrap();
        let (mut c_ok, mut r_end) = (false, false);
        drain_until(&mut io, &mut out, |_, c| {
            if c.token.0 == tok {
                c_ok = true;
            } else if c.token.0 == 54 && !c.more {
                r_end = true;
            }
            c_ok && r_end
        });
    }
    close_socks(&mut io, &mut out, &[Sock(0), Sock(1), srv2]);
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    armed_recv_bytes_and_enobufs_and_eof(make, addr);
}
