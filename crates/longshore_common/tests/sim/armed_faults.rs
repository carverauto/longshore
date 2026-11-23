// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulation tests with network faults over armed accept and armed receive under `alloc_guard`.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{
    BufId, Completion, Completions, Io, Limits, Op, Sock, Token, ECONNRESET, ENFILE, ENOBUFS,
};
use longshore_common::sim::{Faults, SimIo};
use std::net::SocketAddr;

#[global_allocator]
static ALLOC: Guard = Guard;

#[cfg(test)]
#[path = "armed_faults/rig.rs"]
mod rig;
use rig::*;

#[test]
fn armed_echo_survives_delay_short_send_and_partial_recv_with_zero_alloc() {
    let limits = Limits::BASE
        .with_sockets_max(8)
        .with_accept_range_start(1)
        .with_accept_range_len(4)
        .with_buffers(2)
        .with_buffer_size(2048)
        .with_ring_buffers(4)
        .with_ring_buffer_size(256)
        .with_ops_max(16);
    let faults = Faults::NONE
        .with_delay(200_000)
        .with_short_send(300_000)
        .with_partial_recv(300_000);
    let payload: Vec<u8> = (0..4096usize).map(|i| (i * 11 + 7) as u8).collect();
    for seed in 0..30u64 {
        let mut sim = SimIo::new(limits, seed, faults);
        let mut out = Completions::new(8);
        let bound = sim.listen(Sock::new(0), ADDR).unwrap();
        let mut got = Vec::with_capacity(payload.len());
        sealed(|| {
            let srv = armed_accept_and_connect(&mut sim, &mut out, Sock::new(5), bound, true);
            sim.submit(Token::new(3), Op::ArmedRecv { sock: srv })
                .unwrap();
            echo_payload(&mut sim, &mut out, srv, &payload, &mut got);
            close_three(&mut sim, &mut out, [Sock::new(0), Sock::new(5), srv]);
        });
        assert_eq!(got, payload, "seed {seed}");
    }
}

/// Handles a completion of the armed recv (token 3): keeps its bytes and,
/// once it ends, records its final result.
#[cfg(test)]
fn on_armed_recv(sim: &mut SimIo, c: &Completion, got: &mut Vec<u8>, final_res: &mut Option<i32>) {
    if c.result() > 0 {
        take_recv(sim, c, got);
    }
    if !c.more() {
        assert!(c.buf().is_none());
        *final_res = Some(c.result());
    }
}

/// Streams `stream` from Sock(4) to the armed recv (token 3) in 64-byte
/// sends until the recv ends, closing Sock(4) once everything is sent or a
/// send fails; the recv's final result.
#[cfg(test)]
fn stream_until_recv_ends(
    sim: &mut SimIo,
    out: &mut Completions,
    stream: &[u8],
    got: &mut Vec<u8>,
) -> i32 {
    let mut final_res = None;
    let mut sent = 0usize;
    let mut tok = 10u64;
    let mut peer_closed = false;
    while final_res.is_none() {
        if !peer_closed && sent < stream.len() {
            let n = (stream.len() - sent).min(64);
            sim.buf(BufId::new(0))[..n].copy_from_slice(&stream[sent..sent + n]);
            let t = tok;
            tok += 1;
            sim.submit(Token::new(t), send_op(Sock::new(4), n as u32))
                .unwrap();
            let mut sd = false;
            drain_until(sim, out, |sim, c| {
                if c.token().get() == 3 {
                    on_armed_recv(sim, &c, got, &mut final_res);
                } else if c.token().get() == t {
                    if c.result() > 0 {
                        sent += usize::try_from(c.result()).unwrap();
                    } else {
                        peer_closed = true;
                    }
                    sd = true;
                }
                sd || final_res.is_some()
            });
        } else {
            if !peer_closed {
                sim.submit(Token::new(800), Op::Close { sock: Sock::new(4) })
                    .unwrap();
                peer_closed = true;
            }
            drain_until(sim, out, |sim, c| {
                if c.token().get() == 3 {
                    on_armed_recv(sim, &c, got, &mut final_res);
                }
                final_res.is_some()
            });
        }
    }
    final_res.unwrap()
}

#[test]
fn armed_recv_with_resets_and_partial_reads_delivers_prefix_once_and_replays() {
    let limits = Limits::BASE
        .with_sockets_max(6)
        .with_accept_range_start(1)
        .with_accept_range_len(2)
        .with_buffers(2)
        .with_buffer_size(512)
        .with_ring_buffers(2)
        .with_ring_buffer_size(64)
        .with_ops_max(8);
    let faults = Faults::NONE
        .with_reset(50_000)
        .with_partial_recv(300_000)
        .with_delay(200_000);
    let stream: Vec<u8> = (0..512usize).map(|i| (i * 3 + 1) as u8).collect();
    for seed in 0..80u64 {
        let run = || {
            let mut sim = SimIo::new(limits, seed, faults);
            let mut out = Completions::new(8);
            let bound = sim.listen(Sock::new(0), ADDR).unwrap();
            let mut got = Vec::with_capacity(stream.len());
            let mut final_res = None;
            sealed(|| {
                let srv = armed_accept_and_connect(&mut sim, &mut out, Sock::new(4), bound, false);
                sim.submit(Token::new(3), Op::ArmedRecv { sock: srv })
                    .unwrap();
                final_res = Some(stream_until_recv_ends(
                    &mut sim, &mut out, &stream, &mut got,
                ));
            });
            assert_eq!(got.as_slice(), &stream[..got.len()], "seed {seed}");
            let r = final_res.unwrap();
            assert!(r == 0 || r == -ECONNRESET, "seed {seed} res={r}");
            sim.trace_hash()
        };
        assert_eq!(run(), run(), "deterministic replay for seed {seed}");
    }
}

/// A second client, Sock(5), finds the accept range (one socket) full: the
/// armed accept ends with `-ENFILE` and the client's recv with `-ECONNRESET`.
#[cfg(test)]
fn second_client_finds_accept_range_full(
    sim: &mut SimIo,
    out: &mut Completions,
    bound: SocketAddr,
) {
    sim.submit(
        Token::new(3),
        Op::Connect {
            sock: Sock::new(5),
            addr: bound,
        },
    )
    .unwrap();
    let (mut a_full, mut c2) = (false, false);
    drain_until(sim, out, |_, c| {
        if c.token().get() == 1 {
            assert_eq!((c.result(), c.more(), c.buf()), (-ENFILE, false, None));
            a_full = true;
        } else if c.token().get() == 3 {
            c2 = true;
        }
        a_full && c2
    });
    sim.submit(
        Token::new(4),
        Op::Recv {
            sock: Sock::new(5),
            buf: BufId::new(0),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    drain_until(sim, out, |_, c| {
        if c.token().get() == 4 {
            assert_eq!(c.result(), -ECONNRESET);
            return true;
        }
        false
    });
}

/// With its one ring buffer held, the armed recv on `srv` ends with
/// `-ENOBUFS`; given back and re-armed, it delivers the rest, no byte lost.
#[cfg(test)]
fn enobufs_then_refill(sim: &mut SimIo, out: &mut Completions, srv: Sock, got: &mut Vec<u8>) {
    sim.submit(Token::new(5), Op::ArmedRecv { sock: srv })
        .unwrap();
    sim.buf(BufId::new(0))[..16].copy_from_slice(b"0123456789abcdef");
    sim.submit(Token::new(6), send_op(Sock::new(4), 16))
        .unwrap();
    let mut held = None;
    drain_until(sim, out, |sim, c| {
        if c.token().get() == 5 && c.result() > 0 {
            let b = c.buf().unwrap();
            got.extend_from_slice(&sim.buf(b)[..usize::try_from(c.result()).unwrap()]);
            held = Some(b);
            return true;
        }
        false
    });
    sim.buf(BufId::new(0))[..8].copy_from_slice(b"ghijklmn");
    sim.submit(Token::new(7), send_op(Sock::new(4), 8)).unwrap();
    drain_until(sim, out, |_, c| {
        c.token().get() == 5 && !c.more() && c.result() == -ENOBUFS
    });
    sim.give_back(held.unwrap());
    sim.submit(Token::new(8), Op::ArmedRecv { sock: srv })
        .unwrap();
    drain_until(sim, out, |sim, c| {
        if c.token().get() == 8 && c.result() > 0 {
            take_recv(sim, &c, got);
        }
        got.len() == 24
    });
}

#[test]
fn enobufs_and_range_full_recover_under_faults() {
    let limits = Limits::BASE
        .with_sockets_max(8)
        .with_accept_range_start(1)
        .with_accept_range_len(1)
        .with_buffers(2)
        .with_buffer_size(128)
        .with_ring_buffers(1)
        .with_ring_buffer_size(32)
        .with_ops_max(16);
    let faults = Faults::NONE.with_delay(200_000).with_partial_recv(500_000);
    let mut sim = SimIo::new(limits, 7, faults);
    let mut out = Completions::new(8);
    let bound = sim.listen(Sock::new(0), ADDR).unwrap();
    let mut got = Vec::with_capacity(32);
    sealed(|| {
        let srv = armed_accept_and_connect(&mut sim, &mut out, Sock::new(4), bound, false);
        // Second client hits full accept range (len = 1) -> -ENFILE on accept and -ECONNRESET on peer.
        second_client_finds_accept_range_full(&mut sim, &mut out, bound);
        // Exercise -ENOBUFS + refill on srv without losing any byte.
        enobufs_then_refill(&mut sim, &mut out, srv, &mut got);
        assert_eq!(got, b"0123456789abcdefghijklmn");
    });
}
