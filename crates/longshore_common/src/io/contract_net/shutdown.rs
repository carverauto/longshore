// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shutting one direction of a connection down: the peer reads everything
//! sent and then end of stream, and can still send back.

use super::{connect_one, drain_one, net_limits, send_all};
use crate::io::must::{must_len, Must};
use crate::io::{BufId, Completions, Io, Limits, Op, Shut, Sock, Token, ENOTCONN};
use std::net::SocketAddr;

/// Receive on `sock` until `want` bytes have arrived, then return them.
fn recv_exact<IO: Io>(io: &mut IO, out: &mut Completions, sock: Sock, want: usize) -> Vec<u8> {
    let mut got = Vec::new();
    while got.len() < want {
        let op = Op::Recv {
            sock,
            buf: BufId(1),
            off: 0,
            len: 64,
        };
        io.must_submit(Token(50), op);
        let c = drain_one(io, out, 50);
        assert!(c.result > 0, "receive before end of stream: {c:?}");
        got.extend_from_slice(&io.buf(BufId(1))[..must_len(c.result)]);
    }
    got
}

fn recv_once<IO: Io>(io: &mut IO, out: &mut Completions, sock: Sock) -> i32 {
    let op = Op::Recv {
        sock,
        buf: BufId(1),
        off: 0,
        len: 64,
    };
    io.must_submit(Token(51), op);
    drain_one(io, out, 51).result
}

fn shut<IO: Io>(io: &mut IO, out: &mut Completions, sock: Sock, how: Shut) -> i32 {
    io.must_submit(Token(52), Op::Shutdown { sock, how });
    drain_one(io, out, 52).result
}

fn write_side_ends_the_peers_reads_and_leaves_the_other_way_open<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let mut io = make(net_limits());
    let mut out = Completions::new(8);
    let (client, mut tok) = (Sock(1), 100u64);
    let server = connect_one(&mut io, &mut out, addr, Sock(0), client);

    send_all(&mut io, &mut out, client, BufId(0), b"last words", &mut tok);
    assert_eq!(shut(&mut io, &mut out, client, Shut::Write), 0);

    // Everything sent before the shutdown arrives, then end of stream.
    assert_eq!(recv_exact(&mut io, &mut out, server, 10), b"last words");
    assert_eq!(recv_once(&mut io, &mut out, server), 0);

    // The other direction still carries data.
    send_all(&mut io, &mut out, server, BufId(0), b"reply", &mut tok);
    assert_eq!(recv_exact(&mut io, &mut out, client, 5), b"reply");

    // The side that shut its write side down can send no more.
    let op = Op::Send {
        sock: client,
        buf: BufId(0),
        off: 0,
        len: 4,
        zero_copy: false,
    };
    io.must_submit(Token(53), op);
    assert!(drain_one(&mut io, &mut out, 53).result < 0);
}

fn read_side_ends_local_reads<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let mut io = make(net_limits());
    let mut out = Completions::new(8);
    let server = connect_one(&mut io, &mut out, addr, Sock(0), Sock(1));
    assert_eq!(shut(&mut io, &mut out, server, Shut::Read), 0);
    assert_eq!(recv_once(&mut io, &mut out, server), 0);
}

fn a_socket_that_is_not_connected_is_refused<IO: Io>(make: &mut impl FnMut(Limits) -> IO) {
    let mut io = make(net_limits());
    let mut out = Completions::new(8);
    assert_eq!(shut(&mut io, &mut out, Sock(1), Shut::Both), -ENOTCONN);
}

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    write_side_ends_the_peers_reads_and_leaves_the_other_way_open(make, addr);
    read_side_ends_local_reads(make, addr);
    a_socket_that_is_not_connected_is_refused(make);
}
