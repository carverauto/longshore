// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `Faults::with_reorder`: the reports of a closed socket's operations and
//! of the close itself come in every order, by seed, and in the old order
//! when it is off.

use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ECANCELED};
use longshore_common::sim::{Faults, SimIo};
use std::net::SocketAddr;

const LISTENER: Sock = Sock::new(0);
const SERVER: Sock = Sock::new(1);
const CLIENT: Sock = Sock::new(2);
const RECV: Token = Token::new(10);
const CLOSE: Token = Token::new(11);
const ACCEPT: Token = Token::new(12);
const CONNECT: Token = Token::new(13);
/// Virtual time per step of the simulator.
const STEP_NS: u64 = 100_000;

#[cfg(test)]
fn limits() -> Limits {
    Limits::BASE
        .with_sockets_max(3)
        .with_buffers(2)
        .with_buffer_size(256)
        .with_ops_max(8)
}

/// A connected pair with a receive waiting on the server's end, which is
/// then closed: the order its two reports come in, and how much virtual
/// time lay between them.
#[cfg(test)]
fn close_under_a_receive(seed: u64, faults: Faults) -> (Vec<Token>, u64) {
    let addr: SocketAddr = "192.0.2.9:9000".parse().unwrap();
    let mut io = SimIo::new(limits(), seed, faults);
    let _: SocketAddr = io.listen(LISTENER, addr).unwrap();
    let accept = Op::Accept {
        listener: LISTENER,
        sock: SERVER,
    };
    io.submit(ACCEPT, accept).unwrap();
    let connect = Op::Connect { sock: CLIENT, addr };
    io.submit(CONNECT, connect).unwrap();
    let mut out = Completions::new(8);
    let mut up = 0;
    while up < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert!(c.result() >= 0);
            up += 1;
        }
    }
    let recv = Op::Recv {
        sock: SERVER,
        buf: BufId::new(0),
        off: 0,
        len: 64,
    };
    io.submit(RECV, recv).unwrap();
    // The receive is waiting when the close comes.
    io.tick(false, &mut out).unwrap();
    assert!(out.pop().is_none());
    io.submit(CLOSE, Op::Close { sock: SERVER }).unwrap();
    let (mut order, mut first_at) = (Vec::new(), None);
    while order.len() < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() == RECV {
                assert_eq!(c.result(), -ECANCELED);
            } else {
                assert_eq!((c.token(), c.result()), (CLOSE, 0));
            }
            let _: &mut u64 = first_at.get_or_insert(io.now_ns());
            order.push(c.token());
        }
    }
    (order, io.now_ns() - first_at.unwrap())
}

#[test]
fn without_reorder_the_close_reports_first() {
    for seed in 0..200 {
        let (order, apart) = close_under_a_receive(seed, Faults::NONE);
        assert_eq!(order, [CLOSE, RECV], "seed {seed}");
        assert!(apart <= STEP_NS, "seed {seed}: {apart} ns apart");
    }
}

#[test]
fn with_reorder_both_orders_come_and_reports_come_late() {
    let faults = Faults::NONE.with_reorder(500_000);
    let (mut close_first, mut recv_first, mut longest) = (0, 0, 0);
    for seed in 0..200 {
        let (order, apart) = close_under_a_receive(seed, faults);
        match order[..] {
            [CLOSE, RECV] => close_first += 1,
            [RECV, CLOSE] => recv_first += 1,
            _ => panic!("seed {seed}: {order:?}"),
        }
        longest = longest.max(apart);
    }
    assert!(
        close_first > 20 && recv_first > 20,
        "{close_first} and {recv_first}"
    );
    assert!(longest >= 5 * STEP_NS, "the longest gap was {longest} ns");
}

#[test]
fn the_same_seed_gives_the_same_order() {
    let faults = Faults::NONE.with_reorder(500_000);
    for seed in 0..50 {
        assert_eq!(
            close_under_a_receive(seed, faults),
            close_under_a_receive(seed, faults),
            "seed {seed}"
        );
    }
}
