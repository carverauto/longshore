// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the simulator's sockets do at the edges every protocol test leans
//! on: nothing listening, the end of a stream, a full buffer, a read that
//! must not wait, a connect given up, and a simulation that cannot go on.

use longshore_common::io::{
    BufId, Completions, Io, Limits, Op, Sock, Token, EAGAIN, ECANCELED, ECONNREFUSED, EPIPE,
};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const LIMITS: Limits = Limits {
    sockets_max: 5,
    buffers: 3,
    buffer_size: 4096,
    ops_max: 4,
    ..Limits::BASE
};
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const OTHER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)), 443);

const LISTENER: Sock = Sock(0);
/// The accepted end.
const B: Sock = Sock(1);
/// The connecting end.
const A: Sock = Sock(2);

/// No completions.
const NOTHING: [(u64, i32); 0] = [];

struct World {
    io: SimIo,
    out: Completions,
}

impl World {
    fn new(seed: u64) -> World {
        World {
            io: SimIo::new(LIMITS, seed, Faults::default()),
            out: Completions::new(8),
        }
    }

    /// A world with `A` connected to `B`.
    fn connected(seed: u64) -> World {
        let mut w = World::new(seed);
        w.io.listen(LISTENER, ADDR).unwrap();
        let accept = Op::Accept {
            listener: LISTENER,
            sock: B,
        };
        let connect = Op::Connect {
            sock: A,
            addr: ADDR,
        };
        assert_eq!(w.run(&[(1, accept), (2, connect)]), [(1, 0), (2, 0)]);
        w
    }

    /// Submit `ops` and wait for all of them: (token, result), by token.
    fn run(&mut self, ops: &[(u64, Op)]) -> Vec<(u64, i32)> {
        for (t, op) in ops {
            self.io.submit(Token(*t), *op).unwrap();
        }
        let mut seen = vec![];
        while seen.len() < ops.len() {
            self.io.tick(true, &mut self.out).unwrap();
            while let Some(c) = self.out.pop() {
                seen.push((c.token.0, c.result));
            }
        }
        seen.sort();
        seen
    }

    fn one(&mut self, op: Op) -> i32 {
        self.run(&[(9, op)])[0].1
    }

    /// Tick without waiting, `n` times: whatever completed.
    fn idle(&mut self, n: usize) -> Vec<(u64, i32)> {
        let mut seen = vec![];
        for _ in 0..n {
            self.io.tick(false, &mut self.out).unwrap();
            while let Some(c) = self.out.pop() {
                seen.push((c.token.0, c.result));
            }
        }
        seen
    }

    fn send(&mut self, sock: Sock, bytes: &[u8]) -> i32 {
        self.io.buf(BufId(0))[..bytes.len()].copy_from_slice(bytes);
        self.one(Op::Send {
            sock,
            buf: BufId(0),
            off: 0,
            len: bytes.len() as u32,
            zero_copy: false,
        })
    }
}

fn recv(sock: Sock, len: u32) -> Op {
    Op::Recv {
        sock,
        buf: BufId(1),
        off: 0,
        len,
    }
}

fn recv_now(sock: Sock) -> Op {
    Op::RecvNow {
        sock,
        buf: BufId(1),
        off: 0,
        len: 16,
    }
}

#[test]
fn a_connect_is_refused_when_nothing_listens_and_leaves_its_slot_free() {
    let mut w = World::new(1);
    let connect = Op::Connect {
        sock: A,
        addr: ADDR,
    };
    assert_eq!(w.one(connect), -ECONNREFUSED);
    // Listening elsewhere does not help.
    w.io.listen(LISTENER, OTHER).unwrap();
    assert_eq!(w.one(connect), -ECONNREFUSED);
    // The slot needs no close: the same connect works once something listens.
    w.io.listen(Sock(3), ADDR).unwrap();
    let accept = Op::Accept {
        listener: Sock(3),
        sock: B,
    };
    assert_eq!(w.run(&[(1, accept), (2, connect)]), [(1, 0), (2, 0)]);
    assert_eq!(w.send(A, b"hello"), 5);
    assert_eq!(w.one(recv(B, 16)), 5);
    assert_eq!(&w.io.buf(BufId(1))[..5], b"hello");
}

#[test]
fn an_address_is_listened_on_once() {
    let mut w = World::new(1);
    assert_eq!(w.io.listen(LISTENER, ADDR).unwrap(), ADDR);
    let again = w.io.listen(Sock(3), ADDR).unwrap_err();
    assert_eq!(again.kind(), std::io::ErrorKind::AddrInUse);
    assert_eq!(w.io.listen(Sock(3), OTHER).unwrap(), OTHER);
}

#[test]
fn a_closed_stream_delivers_what_was_sent_then_ends() {
    let mut w = World::connected(2);
    assert_eq!(w.send(A, b"hello"), 5);
    assert_eq!(w.one(Op::Close { sock: A }), 0);
    // What was in flight still arrives, then the end of the stream, for good.
    assert_eq!(w.one(recv(B, 3)), 3);
    assert_eq!(&w.io.buf(BufId(1))[..3], b"hel");
    assert_eq!(w.one(recv(B, 16)), 2);
    assert_eq!(&w.io.buf(BufId(1))[..2], b"lo");
    assert_eq!(w.one(recv(B, 16)), 0);
    assert_eq!(w.one(recv(B, 16)), 0);
    // Nobody is left to send to, and the closed slot is no socket at all.
    assert_eq!(w.send(B, b"late"), -EPIPE);
    assert_eq!(w.one(recv(A, 16)), -EPIPE);
    assert_eq!(w.send(A, b"late"), -EPIPE);
}

#[test]
fn a_read_that_must_not_wait_says_what_is_there() {
    let mut w = World::connected(3);
    assert_eq!(w.one(recv_now(B)), -EAGAIN, "nothing has arrived");
    assert_eq!(w.one(recv_now(B)), -EAGAIN, "and asking changes nothing");
    assert_eq!(w.send(A, b"abc"), 3);
    assert_eq!(w.one(recv_now(B)), 3);
    assert_eq!(&w.io.buf(BufId(1))[..3], b"abc");
    assert_eq!(w.one(recv_now(B)), -EAGAIN, "it was consumed");
    assert_eq!(w.one(Op::Close { sock: A }), 0);
    assert_eq!(w.one(recv_now(B)), 0, "the peer has closed");
    assert_eq!(w.one(recv_now(A)), -EPIPE);
}

/// A socket holds 64 KiB in flight. A sender stalls when that is full,
/// resumes with exactly the room a reader makes, and the bytes arrive in
/// order across the wrap of the socket's buffer.
#[test]
fn a_full_socket_stalls_the_sender_until_the_reader_makes_room() {
    let mut w = World::connected(4);
    let chunk = |w: &mut World, k: u8| {
        w.io.buf(BufId(0)).fill(k);
        Op::Send {
            sock: A,
            buf: BufId(0),
            off: 0,
            len: 4096,
            zero_copy: false,
        }
    };
    for k in 0..16 {
        let op = chunk(&mut w, k);
        assert_eq!(w.one(op), 4096, "send {k}");
    }
    let op = chunk(&mut w, 16);
    w.io.submit(Token(7), op).unwrap();
    assert_eq!(
        w.idle(50),
        NOTHING,
        "the seventeenth send has nowhere to go"
    );

    // Room for 100 bytes is room for exactly 100 bytes.
    let mut seen = w.run(&[(8, recv(B, 100))]);
    seen.extend(w.idle(50));
    seen.sort();
    assert_eq!(seen, [(7, 100), (8, 100)]);
    assert!(w.io.buf(BufId(1))[..100].iter().all(|b| *b == 0));

    // Everything else, in the order sent: the rest of chunk 0, chunks 1 to
    // 15, and the 100 bytes of chunk 16 that went in.
    let mut got = Vec::new();
    while got.len() < 16 * 4096 {
        let n = w.one(recv(B, 4096));
        assert!(n > 0);
        got.extend_from_slice(&w.io.buf(BufId(1))[..n as usize]);
    }
    let want: Vec<u8> = (100..16 * 4096)
        .map(|i| (i / 4096) as u8)
        .chain([16u8; 100])
        .collect();
    assert!(got == want, "bytes out of order across the wrap");
    assert_eq!(w.one(recv_now(B)), -EAGAIN, "and nothing more");
}

#[test]
fn a_connect_given_up_before_it_is_accepted_leaves_nothing_to_accept() {
    let mut w = World::new(5);
    w.io.listen(LISTENER, ADDR).unwrap();
    let connect = Op::Connect {
        sock: A,
        addr: ADDR,
    };
    w.io.submit(Token(2), connect).unwrap();
    assert_eq!(w.idle(20), NOTHING, "nobody accepts yet");
    // The cancel finds the connect, and the connect ends as cancelled.
    let mut seen = w.run(&[(3, Op::Cancel { target: Token(2) })]);
    seen.extend(w.idle(20));
    seen.sort();
    assert_eq!(seen, [(2, -ECANCELED), (3, 0)]);
    let accept = Op::Accept {
        listener: LISTENER,
        sock: B,
    };
    w.io.submit(Token(1), accept).unwrap();
    assert_eq!(w.idle(50), NOTHING, "a cancelled connect is not accepted");
    // The slot is free again: a new connect on it meets the waiting accept.
    let mut seen = w.run(&[(4, connect)]);
    seen.extend(w.idle(20));
    seen.sort();
    assert_eq!(seen, [(1, 0), (4, 0)]);
    assert_eq!(w.send(A, b"through"), 7);
    assert_eq!(w.one(recv(B, 16)), 7);
}

/// Waiting for something that cannot happen is reported, not spun on forever.
#[test]
fn waiting_on_what_cannot_happen_is_a_deadlock_error() {
    let mut w = World::connected(7);
    w.io.submit(Token(5), recv(B, 16)).unwrap();
    let e = w.io.tick(true, &mut w.out).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
    assert!(e.to_string().contains("deadlock"), "{e}");
    assert!(w.out.is_empty());
    // A timer among the stuck operations is something that can happen.
    w.io.submit(Token(6), Op::Timeout { nanos: 5_000_000 })
        .unwrap();
    w.io.tick(true, &mut w.out).unwrap();
    assert_eq!(w.out.pop().map(|c| (c.token.0, c.result)), Some((6, 0)));
}

/// Datagrams: one receive takes one datagram and drops what does not fit,
/// a reply goes to whoever sent last, and a send to nobody is not an error.
#[test]
fn datagrams_are_whole_truncated_to_the_receive_and_addressed_to_the_sender() {
    let mut w = World::new(8);
    let (server, client) = (Sock(3), Sock(4));
    w.io.bind_udp(server, ADDR);
    assert_eq!(
        w.one(Op::ConnectUdp {
            sock: client,
            addr: ADDR
        }),
        0
    );
    assert_eq!(w.send(client, b"0123456789"), 10);
    assert_eq!(w.send(client, b"next"), 4);
    assert_eq!(w.one(recv(server, 4)), 4, "the first datagram, cut short");
    assert_eq!(&w.io.buf(BufId(1))[..4], b"0123");
    assert_eq!(w.one(recv(server, 16)), 4, "its tail is gone, not queued");
    assert_eq!(&w.io.buf(BufId(1))[..4], b"next");
    assert_eq!(w.send(server, b"reply"), 5);
    assert_eq!(w.one(recv(client, 16)), 5);
    assert_eq!(&w.io.buf(BufId(1))[..5], b"reply");

    // Nothing is bound at the other address: the datagram is sent and lost.
    let lonely = Sock(2);
    assert_eq!(
        w.one(Op::ConnectUdp {
            sock: lonely,
            addr: OTHER
        }),
        0
    );
    assert_eq!(w.send(lonely, b"anyone?"), 7);
    w.io.submit(Token(5), recv(lonely, 16)).unwrap();
    assert_eq!(w.idle(50), NOTHING, "and no answer comes");
}
