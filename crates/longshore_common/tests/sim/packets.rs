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
    let mut io = SimIo::new(LIMITS, seed, Faults::NONE);
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

/// Send the three packets from `A`, one after the other was taken.
#[cfg(test)]
fn send_all(io: &mut SimIo, out: &mut Completions) {
    for len in PACKETS {
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

/// The lengths of the packets that were sent are kept, in order.
#[test]
fn every_send_is_a_packet_of_its_length() {
    let (mut io, mut out) = connected(1);
    send_all(&mut io, &mut out);
    let mut lens = [0u32; 8];
    assert_eq!(io.last_packets(&mut lens), (3, 3));
    assert_eq!(&lens[..3], &PACKETS);
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
        let all: u32 = PACKETS.iter().sum();
        io.submit(
            RECV,
            Op::ArmedRecvZc {
                sock: B,
                credit: all,
            },
        )
        .unwrap();
        let (mut at, mut ended) = (0u32, false);
        while !ended {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if !c.more() {
                    assert_eq!(c.result(), 0);
                    ended = true;
                    continue;
                }
                let len = c.result().unsigned_abs();
                // The packet this piece begins in, and where that ends.
                let mut end = 0;
                let begins = PACKETS.iter().any(|p| {
                    let begin = end;
                    end += p;
                    begin == at
                });
                let within = PACKETS
                    .iter()
                    .scan(0, |sum, p| {
                        *sum += p;
                        Some(*sum)
                    })
                    .find(|bound| *bound > at)
                    .unwrap();
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
