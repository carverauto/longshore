// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-05`: High-concurrency plain server (armed `Accept`, armed `Recv` ring,
//! 0-idle-buffer invariant, `-ENOBUFS` + `give_back` recovery, shutdown EOF).

use super::super::harness::{drive_until, posix_backend, submit_and_wait_one, write_pool_bytes};
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ENOBUFS};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::net::SocketAddr;

const LISTENER: Sock = Sock(0);
const CLIENT_BASE: u32 = 1;
const IDLE_CLIENTS: u32 = 20;
const ACCEPT_START: u32 = 24;
const ACCEPT_LEN: u32 = 24;
const RING_BUFS: u32 = 16;

fn server_limits() -> Limits {
    Limits {
        sockets_max: 48,
        buffers: 4,
        buffer_size: 2048,
        small_buffers: 2,
        small_buffer_size: 256,
        accept_range_start: ACCEPT_START,
        accept_range_len: ACCEPT_LEN,
        ring_buffers: RING_BUFS,
        ring_buffer_size: 256,
        ops_max: 64,
        ..Limits::BASE
    }
}

fn run_high_concurrency_plain_server<IO: Io>(io: &mut IO, addr: SocketAddr) {
    let mut out = Completions::new(64);
    let bound = io.listen(LISTENER, addr).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: LISTENER })
        .unwrap();

    // 1. Connect 20 idle clients and arm Recv on every accepted socket.
    let mut srv_socks = Vec::with_capacity(IDLE_CLIENTS as usize);
    let mut cli_socks = Vec::with_capacity(IDLE_CLIENTS as usize);
    for i in 0..IDLE_CLIENTS {
        let cs = Sock(CLIENT_BASE + i);
        cli_socks.push(cs);
        io.submit(
            Token(100 + u64::from(i)),
            Op::Connect {
                sock: cs,
                addr: bound,
            },
        )
        .unwrap();
    }

    while srv_socks.len() < IDLE_CLIENTS as usize {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 1 {
                assert!(c.more && c.result >= 0, "accept must remain armed");
                let accepted = Sock(c.result as u32);
                assert!(
                    accepted.0 >= ACCEPT_START && accepted.0 < ACCEPT_START + ACCEPT_LEN,
                    "accepted slot {} outside range",
                    accepted.0
                );
                srv_socks.push(accepted);
            } else if c.token.0 >= 100 && c.token.0 < 200 {
                assert_eq!(c.result, 0);
            }
        }
    }

    for (idx, &ss) in srv_socks.iter().enumerate() {
        io.submit(Token(1000 + idx as u64), Op::ArmedRecv { sock: ss })
            .unwrap();
    }
    // Pump briefly with a short timer while all 20 clients remain silent:
    // zero ring buffers may be consumed (-ENOBUFS must not occur).
    io.submit(Token(500), Op::Timeout { nanos: 1_000_000 })
        .unwrap();
    let idle_cs = drive_until(io, &mut out, 200, |v| v.iter().any(|c| c.token.0 == 500)).unwrap();
    for c in idle_cs {
        if c.token.0 >= 1000 {
            panic!("idle armed recv must not complete or consume buffer: {c:?}");
        }
    }

    // 2. Burst 17 messages without calling give_back -> intentionally exhaust all 16 ring buffers!
    let mut held_bufs: Vec<BufId> = Vec::new();
    let mut ended_socks: Vec<usize> = Vec::new();
    let mut saw_enobufs = false;
    for burst in 0..(RING_BUFS + 2) {
        let msg = [0x60 + burst as u8; 64];
        write_pool_bytes(io, BufId(0), 0, &msg);
        let c_idx = (burst % IDLE_CLIENTS) as usize;
        io.submit(
            Token(2000 + u64::from(burst)),
            Op::Send {
                sock: cli_socks[c_idx],
                buf: BufId(0),
                off: 0,
                len: 64,
                zero_copy: false,
            },
        )
        .unwrap();
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 >= 1000 && c.token.0 < 2000 {
                if c.result > 0 {
                    if let Some(bid) = c.buf {
                        held_bufs.push(bid);
                    }
                } else if c.result == -ENOBUFS {
                    assert!(!c.more, "-ENOBUFS must end armed Recv with more=false");
                    assert!(c.buf.is_none());
                    ended_socks.push((c.token.0 - 1000) as usize);
                    saw_enobufs = true;
                }
            }
        }
        if saw_enobufs {
            break;
        }
    }
    // Drain any remaining completions until -ENOBUFS is observed.
    if !saw_enobufs {
        io.submit(Token(501), Op::Timeout { nanos: 5_000_000 })
            .unwrap();
        let cs = drive_until(io, &mut out, 500, |v| {
            v.iter().any(|c| c.result == -ENOBUFS) || v.iter().any(|c| c.token.0 == 501)
        })
        .unwrap();
        for c in cs {
            if let Some(bid) = c.buf {
                held_bufs.push(bid);
            }
            if c.result == -ENOBUFS {
                assert!(!c.more);
                if c.token.0 >= 1000 && c.token.0 < 2000 {
                    ended_socks.push((c.token.0 - 1000) as usize);
                }
                saw_enobufs = true;
            }
        }
    }
    assert!(
        saw_enobufs,
        "exhausting all {RING_BUFS} ring bufs must yield -ENOBUFS"
    );
    assert_eq!(held_bufs.len(), RING_BUFS as usize);

    // 3. Call give_back on held buffers and verify bytes arrive on re-armed socket.
    for bid in held_bufs.drain(..) {
        io.give_back(bid);
    }
    for idx in ended_socks {
        let _ = io.submit(
            Token(1000 + idx as u64),
            Op::ArmedRecv {
                sock: srv_socks[idx],
            },
        );
    }
    write_pool_bytes(io, BufId(1), 0, b"recovered-after-give-back");
    io.submit(
        Token(3000),
        Op::Send {
            sock: cli_socks[0],
            buf: BufId(1),
            off: 0,
            len: 25,
            zero_copy: false,
        },
    )
    .unwrap();
    let mut got_recovery = false;
    let mut matched_srv_sock = srv_socks[0];
    for _ in 0..200 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if let Some(bid) = c.buf {
                let slice = &io.buf(bid)[..c.result as usize];
                if slice.windows(25).any(|w| w == b"recovered-after-give-back") {
                    if c.token.0 >= 1000 && c.token.0 < 2000 {
                        matched_srv_sock = srv_socks[(c.token.0 - 1000) as usize];
                    }
                    got_recovery = true;
                }
                io.give_back(bid);
            }
        }
        if got_recovery {
            break;
        }
    }
    assert!(
        got_recovery,
        "buffer give_back must restore armed recv flow"
    );

    // 4. Close the matched server socket -> armed recv ends cleanly and peer client sees 0 EOF.
    let _ = submit_and_wait_one(
        io,
        &mut out,
        Token(4000),
        Op::Close {
            sock: matched_srv_sock,
        },
        1_000,
    )
    .unwrap();

    loop {
        let eof_c = submit_and_wait_one(
            io,
            &mut out,
            Token(4001),
            Op::Recv {
                sock: cli_socks[0],
                buf: BufId(2),
                off: 0,
                len: 256,
            },
            1_000,
        )
        .unwrap();
        if eof_c.result == 0 {
            break;
        }
        assert!(eof_c.result > 0);
    }
}

#[cfg(feature = "sim")]
#[test]
fn t4_05_high_concurrency_plain_server_sim() {
    let mut sim = SimIo::new(server_limits(), 405, Faults::default());
    run_high_concurrency_plain_server(&mut sim, "127.0.0.1:9405".parse().unwrap());
}

#[cfg(unix)]
#[test]
fn t4_05_high_concurrency_plain_server_posix() {
    let mut posix = posix_backend(server_limits()).unwrap();
    run_high_concurrency_plain_server(&mut posix, "127.0.0.1:0".parse().unwrap());
}
