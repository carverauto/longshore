// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The simulator's model of a zero-copy send that reports twice
//! (`Op::SendZc`): bytes taken, then, some steps later, the buffer let go
//! of; sends on one socket let go in any order; and a caller that writes
//! bytes still being sent is caught. Under `alloc_guard`.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_buffers(2)
    .with_buffer_size(4096)
    .with_ops_max(8);
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const LISTENER: Sock = Sock::new(0);
const B: Sock = Sock::new(1);
const A: Sock = Sock::new(2);
const SENDS: usize = 4;
const LEN: u32 = 100;
/// What a send of `LEN` bytes reports.
const TAKEN: i32 = 100;

/// A simulator with `A` connected to `B`, and four runs of bytes to send.
#[cfg(test)]
fn connected(seed: u64) -> (SimIo, Completions) {
    let mut io = SimIo::new(LIMITS, seed, Faults::NONE);
    let mut out = Completions::new(8);
    let _: SocketAddr = io.listen(LISTENER, ADDR).unwrap();
    let accept = Op::Accept {
        listener: LISTENER,
        sock: B,
    };
    io.submit(Token::new(1), accept).unwrap();
    let connect = Op::Connect {
        sock: A,
        addr: ADDR,
    };
    io.submit(Token::new(2), connect).unwrap();
    let mut up = 0;
    while up < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result(), 0, "{c:?}");
            up += 1;
        }
    }
    for (i, byte) in io.buf(BufId::new(0)).iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    (io, out)
}

fn send(k: usize) -> Op {
    Op::SendZc {
        sock: A,
        buf: BufId::new(0),
        off: k as u32 * LEN,
        len: LEN,
    }
}

/// What one run of four sends showed, in the order it came.
#[derive(Default, PartialEq, Eq, Debug)]
struct Seen {
    /// (send, more) of every report.
    reports: [(u8, bool); 2 * SENDS],
    count: usize,
    /// Sends that reported twice, and once.
    in_two: u32,
    at_once: u32,
    trace: u64,
}

/// Four sends, each submitted when the one before it was taken, with
/// allocation forbidden from the first send on.
#[cfg(test)]
fn run(seed: u64) -> Seen {
    let (mut io, mut out) = connected(seed);
    let mut seen = Seen::default();
    sealed(|| {
        let (mut first, mut last) = ([false; SENDS], [false; SENDS]);
        let mut next = 0;
        while !last.iter().all(|done| *done) {
            let taken = next == 0 || first[next - 1] || last[next - 1];
            if next < SENDS && taken {
                io.submit(Token::new(10 + next as u64), send(next)).unwrap();
                next += 1;
            }
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                let k = (c.token().get() - 10) as usize;
                assert_eq!(c.result(), TAKEN, "{c:?}");
                assert!(!last[k] && !(c.more() && first[k]), "seed {seed}: {c:?}");
                seen.reports[seen.count] = (k as u8, c.more());
                seen.count += 1;
                if c.more() {
                    first[k] = true;
                } else {
                    seen.in_two += u32::from(first[k]);
                    seen.at_once += u32::from(!first[k]);
                    last[k] = true;
                }
            }
        }
    });
    seen.trace = io.trace_hash();
    seen
}

#[test]
fn a_send_reports_its_bytes_taken_and_later_its_buffer_free() {
    let (mut in_two, mut at_once, mut out_of_order) = (0, 0, 0);
    for seed in 0..300 {
        let seen = run(seed);
        in_two += seen.in_two;
        at_once += seen.at_once;
        let lasts = seen.reports[..seen.count].iter().filter(|r| !r.1);
        out_of_order += u32::from(!lasts.map(|r| r.0).is_sorted());
    }
    // Both shapes a kernel shows, and buffers let go of in another order
    // than they were sent in.
    assert!(
        in_two > 600 && at_once > 50,
        "{in_two} in two, {at_once} at once"
    );
    assert!(out_of_order > 30, "{out_of_order} runs out of order");
}

#[test]
fn the_same_seed_gives_the_same_reports() {
    for seed in [1u64, 7, 4242] {
        assert_eq!(run(seed), run(seed), "seed {seed} diverged");
    }
}

/// The peer reads what was sent though the sender's buffer is still held.
#[test]
fn the_bytes_arrive_before_the_buffer_is_free() {
    let (mut io, mut out) = connected(3);
    io.submit(Token::new(10), send(0)).unwrap();
    let recv = Op::Recv {
        sock: B,
        buf: BufId::new(1),
        off: 0,
        len: LEN,
    };
    io.submit(Token::new(20), recv).unwrap();
    let (mut got, mut lasts) = (0, 0);
    while got < TAKEN || lasts == 0 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            match c.token().get() {
                20 => got += c.result(),
                _ => lasts += u32::from(!c.more()),
            }
        }
        if got < TAKEN && got > 0 {
            panic!("a receive of {got} bytes without faults");
        }
    }
    let (sent, read) = io.buf_pair(BufId::new(0), BufId::new(1));
    assert_eq!(sent[..LEN as usize], read[..LEN as usize]);
    assert_eq!(lasts, 1);
}

/// Writing bytes a send still reads would put other bytes on a real
/// wire: the simulator refuses to go on.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "written before its last completion")]
fn bytes_written_while_they_are_sent_are_caught() {
    for seed in 0..50 {
        let (mut io, mut out) = connected(seed);
        io.submit(Token::new(10), send(0)).unwrap();
        let mut ended = false;
        while !ended {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                ended |= !c.more();
                // Taken, not yet let go of: the caller writes it anyway.
                io.buf(BufId::new(0))[0] ^= 0xff;
            }
        }
    }
}
