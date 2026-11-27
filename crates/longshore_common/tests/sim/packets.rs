// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Packets in the simulator: every send on a stream socket is one, a
//! receive into the receive area cuts its pieces from them, and a packet
//! starts a page unless it was joined to the one before.

use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_buffers(1)
    .with_buffer_size(8192)
    .with_area_pages(16)
    .with_ops_max(8);
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const LISTENER: Sock = Sock::new(0);
const B: Sock = Sock::new(1);
const A: Sock = Sock::new(2);
const RECV: Token = Token::new(20);
/// The packets of a run: three sends, of these lengths.
const PACKETS: [u32; 3] = [700, 5000, 300];

/// A simulator with `A` connected to `B`.
#[cfg(test)]
fn connected(seed: u64) -> (SimIo, Completions) {
    connected_after(seed, false)
}

/// A simulator with `A` connected to `B`, both told before that their
/// sends end a buffer, or not.
#[cfg(test)]
fn connected_after(seed: u64, ends: bool) -> (SimIo, Completions) {
    let mut io = SimIo::new(LIMITS, seed, Faults::NONE);
    io.end_sends(A, ends);
    io.end_sends(B, ends);
    let mut out = Completions::new(16);
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
        while out.pop().is_some() {
            up += 1;
        }
    }
    (io, out)
}

/// Send `lens` from `A`, one packet each, every one taken whole.
#[cfg(test)]
fn send(io: &mut SimIo, out: &mut Completions, lens: &[u32]) {
    for &len in lens {
        let op = Op::Send {
            sock: A,
            buf: BufId::new(0),
            off: 0,
            len,
            zero_copy: false,
        };
        io.submit(Token::new(10), op).unwrap();
        let mut taken = false;
        while !taken {
            io.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                assert_eq!(c.result(), len.cast_signed());
                taken = true;
            }
        }
    }
}

/// Send the three packets from `A`, one after the other was taken.
#[cfg(test)]
fn send_all(io: &mut SimIo, out: &mut Completions) {
    send(io, out, &PACKETS);
}

/// The lengths of the packets that were sent are kept, in order.
#[test]
fn every_send_is_a_packet_of_its_length() {
    let (mut io, mut out) = connected(1);
    send_all(&mut io, &mut out);
    let mut lens = [0u32; 8];
    assert_eq!(io.last_packets(&mut lens), (3, 3));
    assert_eq!(&lens[..3], &PACKETS);
}

/// Receive the packets `lens` into the area at `B`, every piece given
/// back as it comes: how many packets began, and how many of them a page.
#[cfg(test)]
fn receive(io: &mut SimIo, out: &mut Completions, lens: &[u32], seed: u64) -> (u32, u32) {
    let (mut starts, mut at_page_start) = (0, 0);
    let all: u32 = lens.iter().sum();
    let credit = Op::ArmedRecvZc {
        sock: B,
        credit: all,
    };
    io.submit(RECV, credit).unwrap();
    let (mut at, mut ended) = (0u32, false);
    while !ended {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if !c.more() {
                assert_eq!(c.result(), 0);
                ended = true;
                continue;
            }
            let len = c.result().unsigned_abs();
            // Whether this piece begins a packet, and where the packet
            // it lies in ends.
            let bounds = lens.iter().scan(0, |sum, p| {
                *sum += p;
                Some(*sum)
            });
            let begins = at == 0 || bounds.clone().any(|bound| bound == at);
            let within = bounds.clone().find(|bound| *bound > at).unwrap();
            assert!(at + len <= within, "seed {seed}: a piece across packets");
            if begins {
                starts += 1;
                at_page_start += u32::from(c.off() == 0);
            }
            at += len;
            io.give_back_piece(c.buf().unwrap(), c.off(), len);
        }
    }
    assert_eq!(at, all);
    (starts, at_page_start)
}

/// What `runs` seeds of three packets show, the sender's sends ending a
/// buffer or not: packets that began, and how many of them a page.
#[cfg(test)]
fn starts_of_pages(ends: bool) -> (u32, u32) {
    let (mut starts, mut at_page_start) = (0, 0);
    for seed in 0..80 {
        let (mut io, mut out) = connected(seed);
        io.end_sends(A, ends);
        send_all(&mut io, &mut out);
        let (s, p) = receive(&mut io, &mut out, &PACKETS, seed);
        (starts, at_page_start) = (starts + s, at_page_start + p);
    }
    (starts, at_page_start)
}

/// A receive into the area never delivers a piece that runs from one
/// packet into the next, and a packet's first piece starts a page,
/// but for the one in eight that was joined to the packet before.
#[test]
fn pieces_are_cut_from_packets_and_a_packet_starts_a_page() {
    let (starts, at_page_start) = starts_of_pages(false);
    // 240 packets: about seven in eight start a page by being a packet,
    // and one in four of the rest by chance.
    assert!(
        starts == 240 && at_page_start > 200,
        "{at_page_start} of {starts}"
    );
    assert!(at_page_start < 240, "none was joined to the packet before");
}

/// When the sender's sends each end a buffer, no packet is joined to the
/// one before: every one starts a page.
#[test]
fn a_packet_after_a_send_that_ended_a_buffer_always_starts_a_page() {
    assert_eq!(starts_of_pages(true), (240, 240));
}

/// A packet's start, once its bytes were taken, is behind for good: when
/// the socket's queue comes round past the place it lay, it is not taken
/// for the start of a packet again, and the packets that fill the queue
/// still each start a page.
#[test]
fn a_taken_start_stays_behind_when_the_queue_comes_round() {
    // 100 bytes, taken; then 65,500 more, which fill the queue round past
    // where the first packet began.
    let mut after = [8192u32; 8];
    after[7] = 65_500 - 7 * 8192;
    for seed in 0..20 {
        let (mut io, mut out) = connected(seed);
        io.end_sends(A, true);
        send(&mut io, &mut out, &[100]);
        assert_eq!(receive(&mut io, &mut out, &[100], seed), (1, 1));
        send(&mut io, &mut out, &after);
        let (starts, at_page_start) = receive(&mut io, &mut out, &after, seed);
        assert_eq!((starts, at_page_start), (8, 8), "seed {seed}");
    }
}

/// What a socket was told about its sends before it was connected or
/// accepted is not carried into the connection: a new connection's sends
/// end no buffer until it is told so, as on the kernel's backends.
#[test]
fn a_new_connection_does_not_keep_what_its_slot_was_told() {
    let (mut starts, mut at_page_start) = (0, 0);
    for seed in 0..80 {
        let (mut io, mut out) = connected_after(seed, true);
        send_all(&mut io, &mut out);
        let (s, p) = receive(&mut io, &mut out, &PACKETS, seed);
        (starts, at_page_start) = (starts + s, at_page_start + p);
    }
    assert_eq!(starts, 240);
    assert!(at_page_start < 240, "a packet of A's was never joined");
}
