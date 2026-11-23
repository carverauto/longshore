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
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 8080);

fn send_op(sock: Sock, len: u32) -> Op {
    Op::Send {
        sock,
        buf: BufId(0),
        off: 0,
        len,
        zero_copy: false,
    }
}

fn drain_until(
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

#[test]
fn armed_echo_survives_delay_short_send_and_partial_recv_with_zero_alloc() {
    let limits = Limits {
        sockets_max: 8,
        accept_range_start: 1,
        accept_range_len: 4,
        buffers: 2,
        buffer_size: 2048,
        ring_buffers: 4,
        ring_buffer_size: 256,
        ops_max: 16,
        ..Limits::BASE
    };
    let faults = Faults {
        delay: 200_000,
        short_send: 300_000,
        partial_recv: 300_000,
        ..Faults::default()
    };
    let payload: Vec<u8> = (0..4096usize).map(|i| (i * 11 + 7) as u8).collect();
    for seed in 0..30u64 {
        let mut sim = SimIo::new(limits, seed, faults);
        let mut out = Completions::new(8);
        let bound = sim.listen(Sock(0), ADDR).unwrap();
        let mut got = Vec::with_capacity(payload.len());
        sealed(|| {
            sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
                .unwrap();
            sim.submit(
                Token(2),
                Op::Connect {
                    sock: Sock(5),
                    addr: bound,
                },
            )
            .unwrap();
            let mut srv = None;
            let mut connected = false;
            drain_until(&mut sim, &mut out, |_, c| {
                if c.token.0 == 1 {
                    assert!(c.more && c.result >= 0);
                    srv = Some(Sock(u32::try_from(c.result).unwrap()));
                } else if c.token.0 == 2 {
                    assert_eq!(c.result, 0);
                    connected = true;
                }
                srv.is_some() && connected
            });
            let srv = srv.unwrap();
            sim.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();

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
                    drain_until(&mut sim, &mut out, |sim, c| {
                        if c.token.0 == 3 {
                            if c.result > 0 {
                                let b = c.buf.unwrap();
                                got.extend_from_slice(
                                    &sim.buf(b)[..usize::try_from(c.result).unwrap()],
                                );
                                sim.give_back(b);
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
                    drain_until(&mut sim, &mut out, |sim, c| {
                        if c.token.0 == 3 && c.result > 0 {
                            let b = c.buf.unwrap();
                            got.extend_from_slice(
                                &sim.buf(b)[..usize::try_from(c.result).unwrap()],
                            );
                            sim.give_back(b);
                        }
                        got.len() == payload.len()
                    });
                }
            }
            for (k, &s) in [Sock(0), Sock(5), srv].iter().enumerate() {
                sim.submit(Token(900 + k as u64), Op::Close { sock: s })
                    .unwrap();
            }
            let mut closed = 0;
            drain_until(&mut sim, &mut out, |_, c| {
                if c.token.0 >= 900 {
                    closed += 1;
                }
                closed == 3
            });
        });
        assert_eq!(got, payload, "seed {seed}");
    }
}

#[test]
fn armed_recv_with_resets_and_partial_reads_delivers_prefix_once_and_replays() {
    let limits = Limits {
        sockets_max: 6,
        accept_range_start: 1,
        accept_range_len: 2,
        buffers: 2,
        buffer_size: 512,
        ring_buffers: 2,
        ring_buffer_size: 64,
        ops_max: 8,
        ..Limits::BASE
    };
    let faults = Faults {
        reset: 50_000,
        partial_recv: 300_000,
        delay: 200_000,
        ..Faults::default()
    };
    let stream: Vec<u8> = (0..512usize).map(|i| (i * 3 + 1) as u8).collect();
    for seed in 0..80u64 {
        let run = || {
            let mut sim = SimIo::new(limits, seed, faults);
            let mut out = Completions::new(8);
            let bound = sim.listen(Sock(0), ADDR).unwrap();
            let mut got = Vec::with_capacity(stream.len());
            let mut final_res = None;
            sealed(|| {
                sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
                    .unwrap();
                sim.submit(
                    Token(2),
                    Op::Connect {
                        sock: Sock(4),
                        addr: bound,
                    },
                )
                .unwrap();
                let mut srv = None;
                let mut connected = false;
                drain_until(&mut sim, &mut out, |_, c| {
                    if c.token.0 == 1 {
                        srv = Some(Sock(u32::try_from(c.result).unwrap()));
                    } else if c.token.0 == 2 {
                        connected = true;
                    }
                    srv.is_some() && connected
                });
                let srv = srv.unwrap();
                sim.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();
                let mut sent = 0usize;
                let mut tok = 10u64;
                let mut peer_closed = false;
                while final_res.is_none() {
                    if !peer_closed && sent < stream.len() {
                        let n = (stream.len() - sent).min(64);
                        sim.buf(BufId(0))[..n].copy_from_slice(&stream[sent..sent + n]);
                        let t = tok;
                        tok += 1;
                        sim.submit(Token(t), send_op(Sock(4), n as u32)).unwrap();
                        let mut sd = false;
                        drain_until(&mut sim, &mut out, |sim, c| {
                            if c.token.0 == 3 {
                                if c.result > 0 {
                                    let b = c.buf.unwrap();
                                    got.extend_from_slice(
                                        &sim.buf(b)[..usize::try_from(c.result).unwrap()],
                                    );
                                    sim.give_back(b);
                                }
                                if !c.more {
                                    assert!(c.buf.is_none());
                                    final_res = Some(c.result);
                                }
                            } else if c.token.0 == t {
                                if c.result > 0 {
                                    sent += usize::try_from(c.result).unwrap();
                                } else {
                                    peer_closed = true;
                                }
                                sd = true;
                            }
                            sd || final_res.is_some()
                        });
                    } else {
                        if !peer_closed {
                            sim.submit(Token(800), Op::Close { sock: Sock(4) }).unwrap();
                            peer_closed = true;
                        }
                        drain_until(&mut sim, &mut out, |sim, c| {
                            if c.token.0 == 3 {
                                if c.result > 0 {
                                    let b = c.buf.unwrap();
                                    got.extend_from_slice(
                                        &sim.buf(b)[..usize::try_from(c.result).unwrap()],
                                    );
                                    sim.give_back(b);
                                }
                                if !c.more {
                                    assert!(c.buf.is_none());
                                    final_res = Some(c.result);
                                }
                            }
                            final_res.is_some()
                        });
                    }
                }
            });
            assert_eq!(got.as_slice(), &stream[..got.len()], "seed {seed}");
            let r = final_res.unwrap();
            assert!(r == 0 || r == -ECONNRESET, "seed {seed} res={r}");
            sim.trace_hash()
        };
        assert_eq!(run(), run(), "deterministic replay for seed {seed}");
    }
}

#[test]
fn enobufs_and_range_full_recover_under_faults() {
    let limits = Limits {
        sockets_max: 8,
        accept_range_start: 1,
        accept_range_len: 1,
        buffers: 2,
        buffer_size: 128,
        ring_buffers: 1,
        ring_buffer_size: 32,
        ops_max: 16,
        ..Limits::BASE
    };
    let faults = Faults {
        delay: 200_000,
        partial_recv: 500_000,
        ..Faults::default()
    };
    let mut sim = SimIo::new(limits, 7, faults);
    let mut out = Completions::new(8);
    let bound = sim.listen(Sock(0), ADDR).unwrap();
    let mut got = Vec::with_capacity(32);
    sealed(|| {
        sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .unwrap();
        sim.submit(
            Token(2),
            Op::Connect {
                sock: Sock(4),
                addr: bound,
            },
        )
        .unwrap();
        let mut srv = None;
        let mut c1 = false;
        drain_until(&mut sim, &mut out, |_, c| {
            if c.token.0 == 1 {
                srv = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token.0 == 2 {
                c1 = true;
            }
            srv.is_some() && c1
        });
        let srv = srv.unwrap();
        // Second client hits full accept range (len = 1) -> -ENFILE on accept and -ECONNRESET on peer.
        sim.submit(
            Token(3),
            Op::Connect {
                sock: Sock(5),
                addr: bound,
            },
        )
        .unwrap();
        let (mut a_full, mut c2) = (false, false);
        drain_until(&mut sim, &mut out, |_, c| {
            if c.token.0 == 1 {
                assert_eq!((c.result, c.more, c.buf), (-ENFILE, false, None));
                a_full = true;
            } else if c.token.0 == 3 {
                c2 = true;
            }
            a_full && c2
        });
        sim.submit(
            Token(4),
            Op::Recv {
                sock: Sock(5),
                buf: BufId(0),
                off: 0,
                len: 16,
            },
        )
        .unwrap();
        drain_until(&mut sim, &mut out, |_, c| {
            if c.token.0 == 4 {
                assert_eq!(c.result, -ECONNRESET);
                return true;
            }
            false
        });

        // Exercise -ENOBUFS + refill on srv without losing any byte.
        sim.submit(Token(5), Op::ArmedRecv { sock: srv }).unwrap();
        sim.buf(BufId(0))[..16].copy_from_slice(b"0123456789abcdef");
        sim.submit(Token(6), send_op(Sock(4), 16)).unwrap();
        let mut held = None;
        drain_until(&mut sim, &mut out, |sim, c| {
            if c.token.0 == 5 && c.result > 0 {
                let b = c.buf.unwrap();
                got.extend_from_slice(&sim.buf(b)[..usize::try_from(c.result).unwrap()]);
                held = Some(b);
                return true;
            }
            false
        });
        sim.buf(BufId(0))[..8].copy_from_slice(b"ghijklmn");
        sim.submit(Token(7), send_op(Sock(4), 8)).unwrap();
        drain_until(&mut sim, &mut out, |_, c| {
            c.token.0 == 5 && !c.more && c.result == -ENOBUFS
        });
        sim.give_back(held.unwrap());
        sim.submit(Token(8), Op::ArmedRecv { sock: srv }).unwrap();
        drain_until(&mut sim, &mut out, |sim, c| {
            if c.token.0 == 8 && c.result > 0 {
                let b = c.buf.unwrap();
                got.extend_from_slice(&sim.buf(b)[..usize::try_from(c.result).unwrap()]);
                sim.give_back(b);
            }
            got.len() == 24
        });
        assert_eq!(got, b"0123456789abcdefghijklmn");
    });
}
