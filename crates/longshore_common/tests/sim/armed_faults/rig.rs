// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The rig the armed-fault simulation tests share: a simulated server with an
//! armed accept, its clients, an echo exchange and closing the sockets.

use longshore_common::io::{BufId, Completion, Completions, Io, Op, Sock, Token};
use longshore_common::sim::SimIo;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub(crate) const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 8080);

pub(crate) fn send_op(sock: Sock, len: u32) -> Op {
    Op::Send {
        sock,
        buf: BufId(0),
        off: 0,
        len,
        zero_copy: false,
    }
}

#[cfg(test)]
pub(crate) fn drain_until(
    sim: &mut SimIo,
    out: &mut Completions,
    mut pred: impl FnMut(&mut SimIo, Completion) -> bool,
) {
    for _ in 0..50_000 {
        sim.tick(true, out).expect("tick");
        while let Some(c) = out.pop() {
            if pred(sim, c) {
                return;
            }
        }
    }
    panic!("simulation fault test timed out");
}

/// Appends the bytes of a positive armed-recv completion to `got` and gives
/// its ring buffer back.
#[cfg(test)]
pub(crate) fn take_recv(sim: &mut SimIo, c: &Completion, got: &mut Vec<u8>) {
    let b = c.buf.unwrap();
    got.extend_from_slice(&sim.buf(b)[..usize::try_from(c.result).unwrap()]);
    sim.give_back(b);
}

/// Arms an accept on Sock(0) (token 1) and connects `client` to `bound`
/// (token 2); the accepted socket. `strict` also checks that the accept
/// stays armed and the connect succeeds.
#[cfg(test)]
pub(crate) fn armed_accept_and_connect(
    sim: &mut SimIo,
    out: &mut Completions,
    client: Sock,
    bound: SocketAddr,
    strict: bool,
) -> Sock {
    sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    sim.submit(
        Token(2),
        Op::Connect {
            sock: client,
            addr: bound,
        },
    )
    .unwrap();
    let mut srv = None;
    let mut connected = false;
    drain_until(sim, out, |_, c| {
        if c.token.0 == 1 {
            if strict {
                assert!(c.more && c.result >= 0);
            }
            srv = Some(Sock(u32::try_from(c.result).unwrap()));
        } else if c.token.0 == 2 {
            if strict {
                assert_eq!(c.result, 0);
            }
            connected = true;
        }
        srv.is_some() && connected
    });
    srv.unwrap()
}

/// The echo of `payload` from Sock(5) to the armed recv (token 3) on `srv`,
/// one send of at most 2048 bytes at a time, re-arming the recv when it ends.
#[cfg(test)]
pub(crate) fn echo_payload(
    sim: &mut SimIo,
    out: &mut Completions,
    srv: Sock,
    payload: &[u8],
    got: &mut Vec<u8>,
) {
    let mut sent = 0usize;
    let mut tok = 10u64;
    while sent < payload.len() || got.len() < payload.len() {
        if sent < payload.len() {
            let n = (payload.len() - sent).min(2048);
            sim.buf(BufId(0))[..n].copy_from_slice(&payload[sent..sent + n]);
            let t = tok;
            tok += 1;
            sim.submit(Token(t), send_op(Sock(5), n as u32)).unwrap();
            let mut send_done = false;
            drain_until(sim, out, |sim, c| {
                if c.token.0 == 3 {
                    if c.result > 0 {
                        take_recv(sim, &c, got);
                    }
                    if !c.more && got.len() < payload.len() {
                        sim.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();
                    }
                } else if c.token.0 == t {
                    assert!(c.result > 0);
                    sent += usize::try_from(c.result).unwrap();
                    send_done = true;
                }
                send_done
            });
        } else {
            drain_until(sim, out, |sim, c| {
                if c.token.0 == 3 && c.result > 0 {
                    take_recv(sim, &c, got);
                }
                got.len() == payload.len()
            });
        }
    }
}

/// Closes `socks` (tokens from 900) and waits for all three closes.
#[cfg(test)]
pub(crate) fn close_three(sim: &mut SimIo, out: &mut Completions, socks: [Sock; 3]) {
    for (k, &s) in socks.iter().enumerate() {
        sim.submit(Token(900 + k as u64), Op::Close { sock: s })
            .unwrap();
    }
    let mut closed = 0;
    drain_until(sim, out, |_, c| {
        if c.token.0 >= 900 {
            closed += 1;
        }
        closed == 3
    });
}
