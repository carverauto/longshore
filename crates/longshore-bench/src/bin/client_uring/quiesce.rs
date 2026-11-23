// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A ring dropped with operations in flight first stops them all, well
//! inside quiesce's five-second bound: idle receives on connected sockets,
//! timers an hour away, a connect to an address nothing answers and the
//! wake-up read, over 200 at once, and every time.

use crate::die::OrDie;
use longshore::io::uring::UringIo;
use longshore::io::{Completions, Io, Limits, Op, Sock, Token};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::mpsc::{channel, Receiver};

const SOCKS: u32 = 100;
const REPEATS: u32 = 50;

/// Quiesce a ring full of idle operations, `REPEATS` times.
pub fn check(loopback: IpAddr) {
    let listener = std::net::TcpListener::bind((loopback, 0)).or_die("listener");
    let peer = listener.local_addr().or_die("local_addr");
    let (accepted, round_accepted) = channel();
    let (finished, round_finished) = channel::<()>();
    // Holds each round's peers open, so the receives stay idle.
    let acceptor = std::thread::spawn(move || {
        for _ in 0..REPEATS {
            let peers: Vec<_> = (0..SOCKS)
                .map(|_| listener.accept().or_die("accept"))
                .collect();
            accepted.send(()).or_die("send");
            round_finished.recv().or_die("recv");
            drop(peers);
        }
    });
    for round in 0..REPEATS {
        one_round(round, peer, &round_accepted);
        finished.send(()).or_die("send");
    }
    acceptor.join().or_die("acceptor");
}

/// Connect `SOCKS` sockets to `peer`, leave a receive and a timer on each,
/// a connect to nowhere and the wake-up read in flight, and quiesce.
fn one_round(round: u32, peer: SocketAddr, round_accepted: &Receiver<()>) {
    let limits = Limits {
        sockets_max: SOCKS + 1,
        buffers: 1,
        buffer_size: 8192,
        small_buffers: SOCKS,
        small_buffer_size: 64,
        ops_max: 2 * SOCKS + 2,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).or_die("ring");
    let _wake = io.waker().or_die("waker");
    let mut out = Completions::new(SOCKS as usize);
    connect_all(&mut io, peer, &mut out);
    round_accepted.recv().or_die("recv");
    arm_idle(&mut io, limits);
    io.tick(false, &mut out).or_die("submit");
    // A host with no route at all refuses the connect at once.
    while let Some(c) = out.pop() {
        assert_eq!(
            c.token,
            Token(9998),
            "round {round}: an idle operation ended early"
        );
    }
    let started = std::time::Instant::now();
    assert!(
        io.quiesce(),
        "round {round}: operations in flight after quiesce"
    );
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(1),
        "round {round}: quiesce took {took:?}"
    );
    drop(io);
}

/// Connect sockets `0..SOCKS` to `peer` and wait until all have.
fn connect_all(io: &mut UringIo, peer: SocketAddr, out: &mut Completions) {
    for n in 0..SOCKS {
        let to = Op::Connect {
            sock: Sock(n),
            addr: peer,
        };
        io.submit(Token(u64::from(n)), to).or_die("submit");
    }
    let mut connected = 0;
    while connected < SOCKS {
        io.tick(true, out).or_die("connect");
        while let Some(c) = out.pop() {
            assert!(c.result >= 0, "connect failed: {}", c.result);
            connected += 1;
        }
    }
}

/// Submit an idle receive and an hour's timer on every socket, a connect
/// that waits, and the wake-up read.
fn arm_idle(io: &mut UringIo, limits: Limits) {
    for n in 0..SOCKS {
        let (sock, buf) = (Sock(n), limits.small(n));
        let recv = Op::Recv {
            sock,
            buf,
            off: 0,
            len: 64,
        };
        io.submit(Token(1000 + u64::from(n)), recv).or_die("submit");
        let hour = Op::Timeout {
            nanos: 3_600 * 1_000_000_000,
        };
        io.submit(Token(2000 + u64::from(n)), hour).or_die("submit");
    }
    // TEST-NET-1: routed nowhere, so the connect waits.
    let nowhere = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9);
    io.submit(
        Token(9998),
        Op::Connect {
            sock: Sock(SOCKS),
            addr: nowhere,
        },
    )
    .or_die("submit");
    io.submit(Token(9999), Op::AwaitWake).or_die("submit");
}
