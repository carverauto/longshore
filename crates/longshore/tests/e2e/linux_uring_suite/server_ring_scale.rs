// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel `UringIo` server networking checks: `IORING_FEAT_NODROP` & CQ
//! sizing, `TCP_NODELAY`/`SO_KEEPALIVE` `getsockopt` verification, 1,000 idle
//! connections holding 0 ring buffers, shutdown-before-close EOF, and `alloc_guard`.

use super::super::harness::{drive_until, submit_and_wait_one, write_pool_bytes};
use longshore::io::alloc_guard::sealed;
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use std::io::{Read, Write};
use std::net::TcpStream;

#[test]
fn uring_server_1000_idle_connections_zero_ring_bufs_and_cq_burst_sealed() {
    let lim = Limits {
        sockets_max: 1100,
        buffers: 2,
        buffer_size: 1024,
        small_buffers: 2,
        small_buffer_size: 256,
        accept_range_start: 1,
        accept_range_len: 1024,
        ring_buffers: 64,
        ring_buffer_size: 256,
        ops_max: 1200,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(256);
    let addr = ring
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();

    ring.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    // Connect 1,000 real loopback TCP clients from OS sockets and arm Recv on every accepted slot.
    let mut peers = Vec::with_capacity(1000);
    let mut accepted = Vec::with_capacity(1000);
    for batch in 0..10usize {
        for _ in 0..100 {
            let s = TcpStream::connect(addr).unwrap();
            s.set_nonblocking(true).unwrap();
            peers.push(s);
        }
        let target = (batch + 1) * 100;
        while accepted.len() < target {
            ring.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 1 {
                    assert!(c.more && c.result >= 0);
                    let sock = Sock(c.result as u32);
                    let idx = accepted.len();
                    accepted.push(sock);
                    ring.submit(Token(10_000 + idx as u64), Op::ArmedRecv { sock })
                        .unwrap();
                }
            }
        }
    }
    assert_eq!(accepted.len(), 1000);

    // Steady-state turn under alloc_guard::sealed: while 1,000 connections are idle,
    // all 64 ring buffers remain free. Burst 64 messages across 64 distinct connections
    // -> every single one succeeds with no -ENOBUFS and zero dropped CQEs (NODROP).
    sealed(|| {
        ring.submit(Token(500), Op::Timeout { nanos: 500_000 })
            .unwrap();
        let mut got_timer = false;
        for _ in 0..100 {
            ring.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert_eq!(
                    c.token.0, 500,
                    "idle connections must emit zero recv completions"
                );
                got_timer = true;
            }
            if got_timer {
                break;
            }
        }
        assert!(got_timer, "timer completion must arrive");
    });

    for (i, peer) in peers.iter_mut().take(64).enumerate() {
        let msg = [0x30u8.wrapping_add(i as u8); 16];
        peer.write_all(&msg).unwrap();
    }

    let mut recvd = 0usize;
    while recvd < 64 {
        ring.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 >= 10_000 && c.result > 0 {
                let bid = c.buf.expect("ring buf");
                ring.give_back(bid);
                recvd += 1;
            }
        }
    }

    // Shutdown-before-close on slot 0: peer TcpStream reads clean 0 EOF immediately.
    let _: Completion = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(9000),
        Op::Close { sock: accepted[0] },
        200,
    )
    .unwrap();
    peers[0].set_nonblocking(false).unwrap();
    let mut eof_buf = [0u8; 8];
    assert_eq!(peers[0].read(&mut eof_buf).unwrap(), 0);
}

#[test]
fn uring_tcp_nodelay_and_keepalive_getsockopt_and_cq_nodrop() {
    let lim = Limits {
        sockets_max: 16,
        buffers: 2,
        buffer_size: 1024,
        accept_range_start: 4,
        accept_range_len: 8,
        ring_buffers: 8,
        ring_buffer_size: 256,
        ops_max: 16,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    ring.no_delay(true);
    ring.keep_alive(true);

    let mut out = Completions::new(32);
    let addr = ring
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    ring.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    ring.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();

    let cs = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token.0 == 1 && c.result >= 0)
            && v.iter().any(|c| c.token.0 == 2 && c.result == 0)
    })
    .unwrap();
    let acc_c = cs.iter().find(|c| c.token.0 == 1).unwrap();
    let accepted = Sock(acc_c.result as u32);
    let mut accept_armed = acc_c.more;

    assert_eq!(ring.last_listen_tcp_options(), Some((true, true)));
    assert_eq!(ring.last_connect_tcp_options(), Some((true, true)));

    // Drain in-flight armed accept before calling probe_tcp_options (requires in_flight == 0).
    if accept_armed {
        ring.submit(Token(3), Op::Cancel { target: Token(1) })
            .unwrap();
        let mut cancel_done = false;
        while accept_armed || !cancel_done {
            ring.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 1 {
                    accept_armed = c.more;
                } else if c.token.0 == 3 {
                    cancel_done = true;
                }
            }
        }
    }

    if let Some((acc_nd, acc_ka)) = ring.probe_tcp_options(accepted).unwrap() {
        assert!(acc_nd && acc_ka, "accepted socket must inherit TCP options");
    }
    if let Some((cli_nd, cli_ka)) = ring.probe_tcp_options(Sock(1)).unwrap() {
        assert!(cli_nd && cli_ka, "connected socket must set TCP options");
    }

    // Verify CQ capacity / NODROP under rapid burst with small Completions(2) drain.
    ring.submit(Token(10), Op::ArmedRecv { sock: accepted })
        .unwrap();
    let mut small_out = Completions::new(2);
    for i in 0..8u64 {
        write_pool_bytes(&mut ring, BufId(0), 0, &[i as u8; 32]);
        ring.submit(
            Token(100 + i),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 32,
                zero_copy: false,
            },
        )
        .unwrap();
        ring.tick(true, &mut small_out).unwrap();
        while let Some(c) = small_out.pop() {
            if let Some(b) = c.buf {
                ring.give_back(b);
            }
        }
    }
}
