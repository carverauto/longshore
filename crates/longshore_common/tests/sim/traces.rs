// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Pre-split trace hash locks across all 11 `Op` variants, with and without
//! the delay fault.

use longshore_common::io::{
    BufId, Completions, FileId, Io, Limits, Op, Sock, Token, EAGAIN, ECANCELED, ENOENT,
};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const LIMITS: Limits = Limits {
    sockets_max: 6,
    buffers: 4,
    buffer_size: 4096,
    ops_max: 8,
    ..Limits::BASE
};
const TCP_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const UDP_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)), 5353);

const LISTENER: Sock = Sock(0);
const SERVER: Sock = Sock(1);
const CLIENT: Sock = Sock(2);
const UDP_SRV: Sock = Sock(3);
const UDP_CLI: Sock = Sock(4);

#[cfg(test)]
fn drain(io: &mut SimIo, out: &mut Completions, expected: usize) -> Vec<(u64, i32)> {
    let mut got = Vec::with_capacity(expected);
    while got.len() < expected {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            got.push((c.token.0, c.result));
        }
    }
    got
}

#[cfg(test)]
fn exercise_all_ops(seed: u64, faults: Faults) -> u64 {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut out = Completions::new(8);

    // 1. Register file, set contents, listen TCP, bind UDP, get waker.
    let file = io.register_file("creds.txt").unwrap();
    io.set_file(file, Some(b"secret-key"));
    let _: SocketAddr = io.listen(LISTENER, TCP_ADDR).unwrap();
    io.bind_udp(UDP_SRV, UDP_ADDR);
    let waker = io.waker().unwrap();
    waker.wake();

    wake_read_timers_and_connect_udp(&mut io, &mut out, file);
    udp_round_trip_and_tcp_connect(&mut io, &mut out);
    recv_now_and_missing_cancel(&mut io, &mut out);
    stream_payload(&mut io, &mut out);
    close_all(&mut io, &mut out);

    io.trace_hash()
}

#[cfg(test)]
fn wake_read_timers_and_connect_udp(io: &mut SimIo, out: &mut Completions, file: FileId) {
    // 2. Submit AwaitWake, ReadFile, two Timeouts, Cancel (targeting one Timeout), ConnectUdp.
    io.submit(Token(1), Op::AwaitWake).unwrap();
    io.submit(
        Token(2),
        Op::ReadFile {
            file,
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(Token(3), Op::Timeout { nanos: 400_000 }).unwrap();
    io.submit(Token(4), Op::Timeout { nanos: 5_000_000 })
        .unwrap();
    io.submit(Token(5), Op::Cancel { target: Token(4) })
        .unwrap();
    io.submit(
        Token(6),
        Op::ConnectUdp {
            sock: UDP_CLI,
            addr: UDP_ADDR,
        },
    )
    .unwrap();
    let phase1 = drain(io, out, 6);
    assert!(phase1.contains(&(1, 0)));
    assert!(phase1.contains(&(2, 10)));
    assert!(phase1.contains(&(3, 0)));
    assert!(phase1.contains(&(4, -ECANCELED)));
    assert!(phase1.contains(&(5, 0)));
    assert!(phase1.contains(&(6, 0)));
}

#[cfg(test)]
fn udp_round_trip_and_tcp_connect(io: &mut SimIo, out: &mut Completions) {
    // 3. UDP Send + Recv, simultaneously with TCP Accept + Connect.
    io.buf(BufId(1))[..4].copy_from_slice(b"ping");
    io.submit(
        Token(7),
        Op::Send {
            sock: UDP_CLI,
            buf: BufId(1),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(
        Token(8),
        Op::Recv {
            sock: UDP_SRV,
            buf: BufId(2),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(
        Token(9),
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )
    .unwrap();
    io.submit(
        Token(10),
        Op::Connect {
            sock: CLIENT,
            addr: TCP_ADDR,
        },
    )
    .unwrap();
    let phase2 = drain(io, out, 4);
    assert!(phase2.contains(&(7, 4)));
    assert!(phase2.contains(&(8, 4)));
    assert!(phase2.contains(&(9, 0)));
    assert!(phase2.contains(&(10, 0)));
}

#[cfg(test)]
fn recv_now_and_missing_cancel(io: &mut SimIo, out: &mut Completions) {
    // 4. TCP RecvNow (empty -> -EAGAIN), then Send + Recv, plus Cancel of missing token.
    io.submit(
        Token(11),
        Op::RecvNow {
            sock: SERVER,
            buf: BufId(3),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(Token(12), Op::Cancel { target: Token(999) })
        .unwrap();
    let phase3 = drain(io, out, 2);
    assert!(phase3.contains(&(11, -EAGAIN)));
    assert!(phase3.contains(&(12, -ENOENT)));
}

/// Streams a payload from the TCP client to the server with zero-copy sends,
/// re-submitting the remainder until it has all arrived.
#[cfg(test)]
fn stream_payload(io: &mut SimIo, out: &mut Completions) {
    let payload = b"hello-stream-payload";
    let mut sent = 0usize;
    let mut recvd = 0usize;
    let mut next_tok = 20u64;
    io.buf(BufId(0))[..payload.len()].copy_from_slice(payload);
    while recvd < payload.len() {
        let mut submitted = 0usize;
        if sent < payload.len() {
            io.submit(
                Token(next_tok),
                Op::Send {
                    sock: CLIENT,
                    buf: BufId(0),
                    off: sent as u32,
                    len: (payload.len() - sent) as u32,
                    zero_copy: true,
                },
            )
            .unwrap();
            next_tok += 1;
            submitted += 1;
        }
        io.submit(
            Token(next_tok),
            Op::Recv {
                sock: SERVER,
                buf: BufId(3),
                off: recvd as u32,
                len: (payload.len() - recvd) as u32,
            },
        )
        .unwrap();
        let recv_tok = next_tok;
        next_tok += 1;
        submitted += 1;
        for (tok, res) in drain(io, out, submitted) {
            if tok == recv_tok {
                recvd += usize::try_from(res).unwrap();
            } else {
                sent += usize::try_from(res).unwrap();
            }
        }
    }
}

#[cfg(test)]
fn close_all(io: &mut SimIo, out: &mut Completions) {
    // 5. Close all open sockets (no other ops in flight on those sockets).
    for (idx, sock) in [CLIENT, SERVER, LISTENER, UDP_CLI, UDP_SRV]
        .into_iter()
        .enumerate()
    {
        io.submit(Token(100 + idx as u64), Op::Close { sock })
            .unwrap();
    }
    let closed = drain(io, out, 5);
    assert!(closed.iter().all(|(_, r)| *r == 0));
}

#[test]
fn trace_hashes_match_across_all_ops_without_delay() {
    let expected = [
        (1, 7838426690417349226u64),
        (7, 189675162845082480),
        (42, 12861308534940729908),
        (99, 15422325490640172234),
    ];
    for (seed, want) in expected {
        assert_eq!(
            exercise_all_ops(seed, Faults::default()),
            want,
            "seed {seed} without delay"
        );
    }
}

#[test]
fn trace_hashes_match_across_all_ops_with_delay() {
    let faults = Faults {
        delay: 200_000,
        ..Faults::default()
    };
    let expected = [
        (1, 9502771201062475408u64),
        (7, 14816035989744244144),
        (42, 18060134760023850936),
        (99, 8169797375893241630),
    ];
    for (seed, want) in expected {
        assert_eq!(
            exercise_all_ops(seed, faults),
            want,
            "seed {seed} with delay"
        );
    }
}

#[test]
fn trace_hashes_match_across_all_ops_with_delay_and_stream_faults() {
    let faults = Faults {
        delay: 200_000,
        short_send: 250_000,
        partial_recv: 250_000,
        ..Faults::default()
    };
    let expected = [
        (1, 16521434901384262062u64),
        (7, 4788325518576388030),
        (42, 6896785946161256371),
        (99, 17154470614956463324),
    ];
    for (seed, want) in expected {
        assert_eq!(
            exercise_all_ops(seed, faults),
            want,
            "seed {seed} with delay + stream faults"
        );
    }
}
