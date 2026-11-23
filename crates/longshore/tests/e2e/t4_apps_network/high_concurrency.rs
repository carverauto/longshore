// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-05`: High-concurrency plain server (armed `Accept`, armed `Recv` ring,
//! 0-idle-buffer invariant, `-ENOBUFS` + `give_back` recovery, shutdown EOF).

use super::super::harness::{drive_until, posix_backend, submit_and_wait_one, write_pool_bytes};
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token, ENOBUFS};
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

/// Connects 20 idle clients through the armed accept; returns the accepted
/// server sockets and the client sockets.
fn connect_idle_clients<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    bound: SocketAddr,
) -> (Vec<Sock>, Vec<Sock>) {
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
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 1 {
                assert!(c.more && c.result >= 0, "accept must remain armed");
                let accepted = Sock(u32::try_from(c.result).unwrap());
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
    (srv_socks, cli_socks)
}

/// Arms recv on every accepted socket, then pumps a short timer while all
/// clients stay silent: no ring buffer may be consumed.
fn idle_armed_recvs_hold_no_buffers<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    srv_socks: &[Sock],
) {
    for (idx, &ss) in srv_socks.iter().enumerate() {
        io.submit(Token(1000 + idx as u64), Op::ArmedRecv { sock: ss })
            .unwrap();
    }
    io.submit(Token(500), Op::Timeout { nanos: 1_000_000 })
        .unwrap();
    let idle_cs = drive_until(io, out, 200, |v| v.iter().any(|c| c.token.0 == 500)).unwrap();
    for c in idle_cs {
        if c.token.0 >= 1000 {
            panic!("idle armed recv must not complete or consume buffer: {c:?}");
        }
    }
}

/// Ring buffers held and armed recvs ended by `-ENOBUFS` during the burst.
#[derive(Default)]
struct Exhaustion {
    held_bufs: Vec<BufId>,
    ended_socks: Vec<usize>,
    saw_enobufs: bool,
}

/// Bursts up to 18 messages without `give_back`, stopping at the first `-ENOBUFS`.
fn burst_without_give_back<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    cli_socks: &[Sock],
    ex: &mut Exhaustion,
) {
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
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 >= 1000 && c.token.0 < 2000 {
                if c.result > 0 {
                    if let Some(bid) = c.buf {
                        ex.held_bufs.push(bid);
                    }
                } else if c.result == -ENOBUFS {
                    assert!(!c.more, "-ENOBUFS must end armed Recv with more=false");
                    assert!(c.buf.is_none());
                    ex.ended_socks.push((c.token.0 - 1000) as usize);
                    ex.saw_enobufs = true;
                }
            }
        }
        if ex.saw_enobufs {
            break;
        }
    }
}

/// Drains remaining completions until `-ENOBUFS` shows up or a timer fires.
fn drain_until_enobufs<IO: Io>(io: &mut IO, out: &mut Completions, ex: &mut Exhaustion) {
    io.submit(Token(501), Op::Timeout { nanos: 5_000_000 })
        .unwrap();
    let cs = drive_until(io, out, 500, |v| {
        v.iter().any(|c| c.result == -ENOBUFS) || v.iter().any(|c| c.token.0 == 501)
    })
    .unwrap();
    for c in cs {
        if let Some(bid) = c.buf {
            ex.held_bufs.push(bid);
        }
        if c.result == -ENOBUFS {
            assert!(!c.more);
            if c.token.0 >= 1000 && c.token.0 < 2000 {
                ex.ended_socks.push((c.token.0 - 1000) as usize);
            }
            ex.saw_enobufs = true;
        }
    }
}

/// Gives the held buffers back, re-arms the ended recvs, and checks a new
/// message arrives; returns the server socket that received it.
fn recover_with_give_back<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    ex: Exhaustion,
    srv_socks: &[Sock],
    cli_socks: &[Sock],
) -> Sock {
    for bid in ex.held_bufs {
        io.give_back(bid);
    }
    for idx in ex.ended_socks {
        io.submit(
            Token(1000 + idx as u64),
            Op::ArmedRecv {
                sock: srv_socks[idx],
            },
        )
        .expect("re-arm the receive");
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
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if let Some(bid) = c.buf {
                let slice = &io.buf(bid)[..usize::try_from(c.result).unwrap()];
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
    matched_srv_sock
}

/// Closes `srv` (ending its armed recv); the peer client reads to EOF.
fn close_gives_peer_eof<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock, cli: Sock) {
    let _: Completion =
        submit_and_wait_one(io, out, Token(4000), Op::Close { sock: srv }, 1_000).unwrap();

    loop {
        let eof_c = submit_and_wait_one(
            io,
            out,
            Token(4001),
            Op::Recv {
                sock: cli,
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

fn run_high_concurrency_plain_server<IO: Io>(io: &mut IO, addr: SocketAddr) {
    let mut out = Completions::new(64);
    let bound = io.listen(LISTENER, addr).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: LISTENER })
        .unwrap();

    let (srv_socks, cli_socks) = connect_idle_clients(io, &mut out, bound);
    idle_armed_recvs_hold_no_buffers(io, &mut out, &srv_socks);

    // Burst 17+ messages without give_back to exhaust all 16 ring buffers.
    let mut ex = Exhaustion::default();
    burst_without_give_back(io, &mut out, &cli_socks, &mut ex);
    if !ex.saw_enobufs {
        drain_until_enobufs(io, &mut out, &mut ex);
    }
    assert!(
        ex.saw_enobufs,
        "exhausting all {RING_BUFS} ring bufs must yield -ENOBUFS"
    );
    assert_eq!(ex.held_bufs.len(), RING_BUFS as usize);

    let matched_srv_sock = recover_with_give_back(io, &mut out, ex, &srv_socks, &cli_socks);
    close_gives_peer_eof(io, &mut out, matched_srv_sock, cli_socks[0]);
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
