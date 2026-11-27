// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The simulator's model of a zero-copy receive (`Op::ArmedRecvZc`):
//! pieces of pages of the receive area up to a credit, the end of a
//! credit and the end of the stream, a page not written again before its
//! last piece is back, and a piece returned twice or never delivered
//! caught. Under `alloc_guard`.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Shut, Sock, Token, ENOBUFS};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

#[path = "recv_zc/faults.rs"]
mod faults;

const PAGES: u32 = 8;
const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_buffers(1)
    .with_buffer_size(32 << 10)
    .with_area_pages(PAGES)
    .with_ops_max(8);
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const LISTENER: Sock = Sock::new(0);
const B: Sock = Sock::new(1);
const A: Sock = Sock::new(2);
const SEND: Token = Token::new(10);
const RECV: Token = Token::new(20);

/// A simulator with `A` connected to `B`.
#[cfg(test)]
fn connected(limits: Limits, seed: u64, faults: Faults) -> (SimIo, Completions) {
    let mut io = SimIo::new(limits, seed, faults);
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

/// A piece as a completion gave it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Piece {
    page: BufId,
    off: u32,
    len: u32,
}

/// Send `len` bytes from `A`, byte `i` being `i % 251`.
#[cfg(test)]
fn send(io: &mut SimIo, len: u32) {
    let op = Op::Send {
        sock: A,
        buf: BufId::new(0),
        off: 0,
        len,
        zero_copy: false,
    };
    io.submit(SEND, op).unwrap();
}

/// Arm a receive of `credit` bytes on `B` and turn until it ends. The
/// pieces it brought, each checked to continue the stream at `*at`, and
/// its last result.
#[cfg(test)]
fn receive(
    io: &mut SimIo,
    out: &mut Completions,
    credit: u32,
    at: &mut usize,
    pieces: &mut [Piece; 4096],
    count: &mut usize,
) -> i32 {
    io.submit(RECV, Op::ArmedRecvZc { sock: B, credit })
        .unwrap();
    for _ in 0..100_000 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() != RECV {
                continue;
            }
            if !c.more() {
                return c.result();
            }
            let (page, off, len) = (c.buf().unwrap(), c.off(), c.result().unsigned_abs());
            assert!(LIMITS.is_page(page) && off + len <= Limits::PAGE, "{c:?}");
            let bytes = &io.buf(page)[off as usize..(off + len) as usize];
            for (k, byte) in bytes.iter().enumerate() {
                assert_eq!(
                    *byte,
                    ((*at + k) % 251) as u8,
                    "byte {} of the stream",
                    *at + k
                );
            }
            *at += len as usize;
            pieces[*count] = Piece { page, off, len };
            *count += 1;
        }
    }
    panic!("the receive never ended");
}

/// A header's credit and then the rest of a frame's, frame after frame:
/// each receive brings exactly its credit and ends with 0, the bytes are
/// the stream's in order, and with every piece back nothing is out.
#[test]
fn a_credit_is_delivered_exactly_and_the_stream_goes_on_at_the_next_byte() {
    let mut split = 0;
    for seed in 0..60 {
        let faults = if seed % 2 == 0 {
            Faults::NONE
        } else {
            Faults::NONE.with_partial_recv(500_000)
        };
        let (mut io, mut out) = connected(LIMITS, seed, faults);
        let mut pieces = [Piece {
            page: BufId::new(0),
            off: 0,
            len: 0,
        }; 4096];
        sealed(|| {
            send(&mut io, 3 * 5020);
            let mut at = 0usize;
            for frame in 0..3 {
                let mut header_page = None;
                for credit in [256u32, 5020 - 256] {
                    let (mut count, before) = (0, at);
                    let ended =
                        receive(&mut io, &mut out, credit, &mut at, &mut pieces, &mut count);
                    assert_eq!(
                        (ended, at - before),
                        (0, credit as usize),
                        "seed {seed} frame {frame}"
                    );
                    // The body goes on in the header's page: two pieces
                    // of one page, of two receives.
                    split += usize::from(header_page == Some(pieces[0].page));
                    header_page = Some(pieces[count - 1].page);
                    for p in &pieces[..count] {
                        io.give_back_piece(p.page, p.off, p.len);
                    }
                }
            }
            assert_eq!(io.pieces_out(), (0, 0), "seed {seed}: pieces, pages out");
        });
    }
    assert!(
        split > 60,
        "a body rarely went on in its header's page: {split}"
    );
}

/// The peer finishes inside a credit: the receive ends with 0 and fewer
/// bytes than it was armed for; armed again, it ends with 0 and none.
#[test]
fn the_end_of_the_stream_is_told_from_the_end_of_a_credit_by_the_bytes() {
    let (mut io, mut out) = connected(LIMITS, 7, Faults::NONE);
    let mut pieces = [Piece {
        page: BufId::new(0),
        off: 0,
        len: 0,
    }; 4096];
    send(&mut io, 1000);
    while out.pop().is_none() {
        io.tick(true, &mut out).unwrap();
    }
    let shut = Op::Shutdown {
        sock: A,
        how: Shut::Write,
    };
    io.submit(Token::new(30), shut).unwrap();
    let (mut at, mut count) = (0, 0);
    let ended = receive(&mut io, &mut out, 4096, &mut at, &mut pieces, &mut count);
    assert_eq!((ended, at), (0, 1000));
    let again = receive(&mut io, &mut out, 256, &mut at, &mut pieces, &mut count);
    assert_eq!((again, at), (0, 1000));
}

/// A page is not written again while a piece of it is out, and a piece's
/// bytes are overwritten when it goes back.
#[test]
fn a_page_lives_until_its_last_piece_is_back_and_a_returned_piece_is_marked() {
    let (mut io, mut out) = connected(LIMITS, 3, Faults::NONE);
    let mut pieces = [Piece {
        page: BufId::new(0),
        off: 0,
        len: 0,
    }; 4096];
    // Whole patterns of 251 bytes a send, so the stream's bytes go on.
    const SENT: u32 = 24 * 251;
    send(&mut io, SENT);
    let (mut at, mut count) = (0, 0);
    // The first hundred bytes are kept; what follows fills every page of
    // the area more than once over.
    assert_eq!(
        receive(&mut io, &mut out, 100, &mut at, &mut pieces, &mut count),
        0
    );
    let kept = pieces[0];
    for round in 0..40 {
        if round > 0 {
            send(&mut io, SENT);
        }
        let (first, want) = (count, if round == 0 { SENT - 100 } else { SENT });
        assert_eq!(
            receive(&mut io, &mut out, want, &mut at, &mut pieces, &mut count),
            0
        );
        for p in &pieces[first..count] {
            io.give_back_piece(p.page, p.off, p.len);
        }
        count = first;
        let still = &io.buf(kept.page)[kept.off as usize..(kept.off + kept.len) as usize];
        assert!(
            still.iter().enumerate().all(|(k, b)| *b == (k % 251) as u8),
            "round {round}"
        );
    }
    assert_eq!(io.pieces_out(), (1, 1));
    io.give_back_piece(kept.page, kept.off, kept.len);
    let after = &io.buf(kept.page)[kept.off as usize..(kept.off + kept.len) as usize];
    assert!(
        after.iter().all(|b| *b == 0xDD),
        "a returned piece was not marked"
    );
    assert_eq!(io.pieces_out(), (0, 0));
}

/// With every page behind a piece that is out, nothing more is delivered;
/// it goes on when pieces come back.
#[test]
fn nothing_is_delivered_while_no_page_is_free() {
    let (mut io, mut out) = connected(LIMITS, 11, Faults::NONE);
    let mut pieces = [Piece {
        page: BufId::new(0),
        off: 0,
        len: 0,
    }; 4096];
    let (mut at, mut count) = (0usize, 0);
    let all = PAGES * Limits::PAGE;
    send(&mut io, all.min(32 << 10));
    io.submit(
        RECV,
        Op::ArmedRecvZc {
            sock: B,
            credit: 1 << 20,
        },
    )
    .unwrap();
    let mut quiet = 0;
    while quiet < 200 {
        io.tick(false, &mut out).unwrap();
        quiet += 1;
        while let Some(c) = out.pop() {
            if c.token() == RECV {
                assert!(c.more(), "{c:?}");
                pieces[count] = Piece {
                    page: c.buf().unwrap(),
                    off: c.off(),
                    len: c.result().unsigned_abs(),
                };
                (count, at, quiet) = (count + 1, at + c.result().unsigned_abs() as usize, 0);
            } else if at < 48 << 10 {
                send(&mut io, 16 << 10);
            }
        }
    }
    let (out_pieces, out_pages) = io.pieces_out();
    // The pool is three quarters of the area: the rest is the kernel's
    // own, for what it copies.
    let pool = (PAGES - PAGES / 4) as usize;
    assert_eq!((out_pieces, out_pages), (count, pool), "every page is out");
    for p in &pieces[..count] {
        io.give_back_piece(p.page, p.off, p.len);
    }
    // A pool found empty is given nothing for a while after.
    let before = at;
    for _ in 0..2000 {
        io.tick(false, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() == RECV && c.more() {
                at += c.result().unsigned_abs() as usize;
                io.give_back_piece(c.buf().unwrap(), c.off(), c.result().unsigned_abs());
            }
        }
    }
    assert!(at > before, "nothing came after the pieces went back");
}

#[test]
#[should_panic(expected = "a piece returned that is not out")]
fn a_piece_returned_twice_stops_the_run() {
    let (mut io, mut out) = connected(LIMITS, 5, Faults::NONE);
    let mut pieces = [Piece {
        page: BufId::new(0),
        off: 0,
        len: 0,
    }; 4096];
    send(&mut io, 500);
    let (mut at, mut count) = (0, 0);
    let _ended: i32 = receive(&mut io, &mut out, 500, &mut at, &mut pieces, &mut count);
    let p = pieces[0];
    io.give_back_piece(p.page, p.off, p.len);
    io.give_back_piece(p.page, p.off, p.len);
}

#[test]
#[should_panic(expected = "a piece returned that is not out")]
fn a_piece_of_another_length_stops_the_run() {
    let (mut io, mut out) = connected(LIMITS, 5, Faults::NONE);
    let mut pieces = [Piece {
        page: BufId::new(0),
        off: 0,
        len: 0,
    }; 4096];
    send(&mut io, 500);
    let (mut at, mut count) = (0, 0);
    let _ended: i32 = receive(&mut io, &mut out, 500, &mut at, &mut pieces, &mut count);
    let p = pieces[0];
    io.give_back_piece(p.page, p.off, p.len - 1);
}

#[test]
#[should_panic(expected = "never armed with a credit of 0")]
fn a_credit_of_nothing_stops_the_run() {
    let (mut io, _out) = connected(LIMITS, 5, Faults::NONE);
    let _refused = io.submit(RECV, Op::ArmedRecvZc { sock: B, credit: 0 });
}

/// A backend built without an area refuses the receive.
#[test]
fn a_backend_without_an_area_refuses() {
    let (mut io, mut out) = connected(LIMITS.with_area_pages(0), 5, Faults::NONE);
    io.submit(
        RECV,
        Op::ArmedRecvZc {
            sock: B,
            credit: 100,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    let c = out.pop().unwrap();
    assert_eq!((c.token(), c.result(), c.more()), (RECV, -ENOBUFS, false));
}
