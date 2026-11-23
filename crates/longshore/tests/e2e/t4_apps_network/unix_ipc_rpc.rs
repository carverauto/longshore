// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-06`: Unix-domain socket local IPC RPC server & client (`alloc_guard`
//! sealed steady-state, armed `Accept`/`Recv`, `Cancel`, shutdown-before-close EOF).

use super::super::harness::{
    posix_backend, read_pool_into, write_nul_path, write_pool_bytes, TempDir,
};
use longshore::io::alloc_guard::sealed;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token, ECANCELED};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use longshore::stream::{Step, Stream};

fn submit_wait_one_stack<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    op: Op,
    max_ticks: usize,
) -> Completion {
    io.submit(token, op).unwrap();
    for _ in 0..max_ticks {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() == token {
                return c;
            }
        }
    }
    panic!("submit_wait_one_stack timed out for {token:?}");
}

const UNIX_LISTENER: Sock = Sock::new(0);
const CLI_A: Sock = Sock::new(1);
const CLI_B: Sock = Sock::new(2);
const ACCEPT_START: u32 = 4;
const ACCEPT_LEN: u32 = 4;

fn ipc_limits() -> Limits {
    Limits::BASE
        .with_sockets_max(8)
        .with_buffers(4)
        .with_buffer_size(1024)
        .with_small_buffers(2)
        .with_small_buffer_size(256)
        .with_accept_range_start(ACCEPT_START)
        .with_accept_range_len(ACCEPT_LEN)
        .with_ring_buffers(8)
        .with_ring_buffer_size(256)
        .with_ops_max(16)
}

/// Arms accept and connects both clients; returns the two accepted sockets.
fn accept_both_clients<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    path_buf: BufId,
    plen: u32,
) -> [Sock; 2] {
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: UNIX_LISTENER,
        },
    )
    .unwrap();
    for (token, sock) in [(Token::new(2), CLI_A), (Token::new(3), CLI_B)] {
        io.submit(
            token,
            Op::ConnectUnix {
                sock,
                path_buf,
                path_off: 0,
                path_len: plen,
            },
        )
        .unwrap();
    }

    let mut srv_socks = [Sock::new(0); 2];
    let mut srv_count = 0usize;
    let mut conn_a = false;
    let mut conn_b = false;

    while srv_count < 2 || !conn_a || !conn_b {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            match c.token().get() {
                1 => {
                    assert!(c.more() && c.result() >= 0);
                    srv_socks[srv_count] = Sock::new(u32::try_from(c.result()).unwrap());
                    srv_count += 1;
                }
                2 => {
                    assert_eq!(c.result(), 0);
                    conn_a = true;
                }
                3 => {
                    assert_eq!(c.result(), 0);
                    conn_b = true;
                }
                _ => {}
            }
        }
    }
    srv_socks
}

/// The server echoes the `n` request bytes in ring buffer `bid` back on `srv`.
fn echo_request<IO: Io>(io: &mut IO, bid: BufId, n: u32, srv: Sock) {
    let mut tmp = [0u8; 64];
    read_pool_into(io, bid, 0, &mut tmp[..n as usize]);
    io.give_back(bid);
    write_pool_bytes(io, BufId::new(2), 0, &tmp[..n as usize]);
    io.submit(
        Token::new(30),
        Op::Send {
            sock: srv,
            buf: BufId::new(2),
            off: 0,
            len: n,
            zero_copy: false,
        },
    )
    .unwrap();
}

/// Client A sends an RPC request framed via `Stream` and gets it echoed back.
fn rpc_round_trip<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock) {
    let mut cli_a_stream = Stream::new(CLI_A, Token::new(20), Token::new(21));
    let req = b"RPC:PING:0001";
    write_pool_bytes(io, BufId::new(0), 0, req);
    cli_a_stream.send(io, BufId::new(0), 0, req.len() as u32, u32::MAX);
    assert_eq!(cli_a_stream.recv(io, BufId::new(1), 0, 64), Step::Pending);

    let mut got_resp = false;
    while !got_resp {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 10 && c.result() > 0 {
                let bid = c.buf().expect("ring buf");
                echo_request(io, bid, u32::try_from(c.result()).unwrap(), srv);
            } else if c.token().get() == 21 {
                if let Step::Done(n) = cli_a_stream.on_recv(io, c.result()) {
                    assert_eq!(n, i32::try_from(req.len()).unwrap());
                    let mut got = [0u8; 13];
                    read_pool_into(io, BufId::new(1), 0, &mut got);
                    assert_eq!(&got, req);
                    got_resp = true;
                }
            } else if c.token().get() == 20 {
                let _: Step = cli_a_stream.on_send(io, c.result());
            }
        }
    }
}

/// `Cancel` on an idle RPC recv on client B ends it with `-ECANCELED`.
fn cancel_idle_recv<IO: Io>(io: &mut IO, out: &mut Completions) {
    io.submit(
        Token::new(40),
        Op::Recv {
            sock: CLI_B,
            buf: BufId::new(3),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(
        Token::new(41),
        Op::Cancel {
            target: Token::new(40),
        },
    )
    .unwrap();
    let mut cancel_ok = false;
    let mut recv_canceled = false;
    while !(cancel_ok && recv_canceled) {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 40 {
                assert_eq!(c.result(), -ECANCELED);
                assert!(!c.more());
                recv_canceled = true;
            } else if c.token().get() == 41 {
                assert_eq!(c.result(), 0);
                cancel_ok = true;
            }
        }
    }
}

/// Closing `srv` ends its armed recv cleanly and client B sees 0 EOF.
fn close_gives_client_b_eof<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock) {
    let _: Completion =
        submit_wait_one_stack(io, out, Token::new(50), Op::Close { sock: srv }, 1_000);
    let eof = submit_wait_one_stack(
        io,
        out,
        Token::new(51),
        Op::Recv {
            sock: CLI_B,
            buf: BufId::new(3),
            off: 0,
            len: 64,
        },
        1_000,
    );
    assert_eq!(eof.result(), 0);
}

/// Clean shutdown of the remaining sockets (stack-only completion tracking).
fn close_remaining<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock) {
    for (i, s) in [CLI_A, CLI_B, srv, UNIX_LISTENER].iter().enumerate() {
        io.submit(Token::new(60 + i as u64), Op::Close { sock: *s })
            .unwrap();
    }
    let mut closed_mask = 0u8;
    for _ in 0..200 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if (60..64).contains(&c.token().get()) {
                closed_mask |= 1u8 << (c.token().get() - 60);
            }
        }
        if closed_mask == 0b1111 {
            break;
        }
    }
    assert_eq!(closed_mask, 0b1111, "all 4 closes must complete");
}

fn run_unix_ipc_rpc<IO: Io>(io: &mut IO, lim: Limits, socket_path: &str) {
    let mut out = Completions::new(16);
    io.listen_unix(UNIX_LISTENER, socket_path).unwrap();
    let path_buf = lim.small(0);
    let plen = write_nul_path(io, path_buf, 0, socket_path);

    sealed(|| {
        let srv_socks = accept_both_clients(io, &mut out, path_buf, plen);

        // Arm Recv on both accepted Unix sockets.
        io.submit(Token::new(10), Op::ArmedRecv { sock: srv_socks[0] })
            .unwrap();
        io.submit(Token::new(11), Op::ArmedRecv { sock: srv_socks[1] })
            .unwrap();

        rpc_round_trip(io, &mut out, srv_socks[0]);
        cancel_idle_recv(io, &mut out);
        close_gives_client_b_eof(io, &mut out, srv_socks[1]);
        close_remaining(io, &mut out, srv_socks[0]);
    });
}

#[cfg(feature = "sim")]
#[test]
fn t4_06_unix_ipc_rpc_sealed_sim() {
    let lim = ipc_limits();
    let mut sim = SimIo::new(lim, 406, Faults::default());
    run_unix_ipc_rpc(&mut sim, lim, "/tmp/t4_06_sim.sock");
}

#[cfg(unix)]
#[test]
fn t4_06_unix_ipc_rpc_sealed_posix() {
    let tmp = TempDir::new("t4_06_posix").unwrap();
    let lim = ipc_limits();
    let mut posix = posix_backend(lim).unwrap();
    run_unix_ipc_rpc(
        &mut posix,
        lim,
        tmp.socket_path("rpc.sock").to_str().unwrap(),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn t4_06_unix_ipc_rpc_sealed_uring() {
    let tmp = TempDir::new("t4_06_uring").unwrap();
    let lim = ipc_limits();
    let mut uring = UringIo::new(lim).unwrap();
    run_unix_ipc_rpc(
        &mut uring,
        lim,
        tmp.socket_path("rpc.sock").to_str().unwrap(),
    );
}
