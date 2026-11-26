// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The area's own faults: the kernel's copy path running out of pages,
//! a full completion ring, and that a seed decides all of it.

use super::*;
use longshore_common::io::{Completion, ECANCELED, ENOMEM, ENOSPC};

/// What one receive brought when its pieces are returned as they come:
/// bytes, and its last result.
#[cfg(test)]
fn drain(io: &mut SimIo, out: &mut Completions, credit: u32, at: &mut usize) -> (u32, i32) {
    io.submit(RECV, Op::ArmedRecvZc { sock: B, credit })
        .unwrap();
    let mut got = 0;
    for _ in 0..100_000 {
        io.tick(false, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() != RECV {
                continue;
            }
            if !c.more() {
                return (got, c.result());
            }
            let len = checked(io, c, *at);
            (*at, got) = (*at + len as usize, got + len);
        }
    }
    (got, i32::MAX)
}

/// Check that the piece `c` goes on the stream at `at`, and return it:
/// its length.
#[cfg(test)]
fn checked(io: &mut SimIo, c: Completion, at: usize) -> u32 {
    let (page, off, len) = (c.buf().unwrap(), c.off(), c.result().unsigned_abs());
    let bytes = &io.buf(page)[off as usize..(off + len) as usize];
    for (k, byte) in bytes.iter().enumerate() {
        assert_eq!(*byte, ((at + k) % 251) as u8, "byte {}", at + k);
    }
    io.give_back_piece(page, off, len);
    len
}

/// Payload the kernel copied lies in pages of its own free list, a
/// quarter of the area: with those out the receive ends for want of a
/// page, again and again; with them back it goes on, some time later.
#[test]
fn the_copy_path_runs_out_of_pages_and_gets_them_back() {
    let faults = Faults::NONE.with_copied(1_000_000);
    let (mut io, mut out) = connected(LIMITS, 13, faults);
    let mut pieces = [Piece {
        page: BufId::new(0),
        off: 0,
        len: 0,
    }; 4096];
    const SENT: u32 = 80 * 251;
    send(&mut io, SENT);
    let (mut at, mut count) = (0, 0);
    // Two pages are the list's: two pieces, each at the start of its own.
    let ended = receive(&mut io, &mut out, SENT, &mut at, &mut pieces, &mut count);
    assert_eq!((ended, count, at), (-ENOMEM, 2, 2 * Limits::PAGE as usize));
    assert!(pieces[..2].iter().all(|p| p.off == 0) && pieces[0].page != pieces[1].page);
    // Armed again with the pieces still out: the same, with no byte.
    for _ in 0..20 {
        assert_eq!(drain(&mut io, &mut out, SENT, &mut at), (0, -ENOMEM));
    }
    for p in &pieces[..2] {
        io.give_back_piece(p.page, p.off, p.len);
    }
    // With them back the list fills again, when its wait is over.
    let (mut refused, mut left) = (0, SENT - 2 * Limits::PAGE);
    while left > 0 {
        let (got, last) = drain(&mut io, &mut out, left, &mut at);
        assert!(last == 0 || last == -ENOMEM, "{last}");
        (left, refused) = (left - got, refused + u32::from(last != 0));
        assert!(refused < 10_000, "the copy path never got a page back");
    }
    assert_eq!(io.pieces_out(), (0, 0));
}

/// A receive that finds the completions full ends with `-ENOSPC`, or,
/// having reported a piece, falls silent with bytes still in the socket:
/// until it is cancelled and armed again, or until its peer sends more.
/// The backend says that a tick found them full. Nothing of the stream
/// is lost or doubled either way.
#[test]
fn a_full_completion_ring_ends_a_receive_or_leaves_it_silent() {
    let (mut no_room, mut silences, mut woken) = (0, 0, 0);
    for seed in 0..40 {
        let faults = Faults::NONE.with_ring_full(200_000);
        let (mut io, mut out) = connected(LIMITS, seed, faults);
        const SENT: u32 = 60 * 251;
        send(&mut io, SENT);
        let (mut at, mut left, mut said_full) = (0usize, SENT, false);
        io.submit(
            RECV,
            Op::ArmedRecvZc {
                sock: B,
                credit: left,
            },
        )
        .unwrap();
        let (mut quiet, mut armed) = (0, true);
        while left > 0 {
            io.tick(false, &mut out).unwrap();
            said_full |= io.ring_was_full();
            quiet += 1;
            while let Some(c) = out.pop() {
                if c.token() != RECV {
                    continue;
                }
                quiet = 0;
                if c.more() {
                    let len = checked(&mut io, c, at);
                    (at, left) = (at + len as usize, left - len);
                    continue;
                }
                armed = false;
                no_room += u32::from(c.result() == -ENOSPC);
                assert!(
                    [0, -ENOSPC, -ECANCELED].contains(&c.result()),
                    "seed {seed}: {c:?}"
                );
            }
            if !armed && left > 0 {
                io.submit(
                    RECV,
                    Op::ArmedRecvZc {
                        sock: B,
                        credit: left,
                    },
                )
                .unwrap();
                armed = true;
            }
            if armed && quiet > 600 {
                // Silent with bytes in the socket: told so by the backend.
                assert!(said_full, "seed {seed}: silent, and no tick said full");
                (silences, quiet) = (silences + 1, 0);
                if seed % 2 == 0 {
                    io.submit(Token::new(40), Op::Cancel { target: RECV })
                        .unwrap();
                } else {
                    // The peer's next bytes wake it: one more pattern.
                    send(&mut io, 251);
                    (left, woken) = (left + 251, woken + 1);
                }
            }
        }
        assert_eq!(at % 251, 0, "seed {seed}");
    }
    assert!(
        no_room > 5 && silences > 5 && woken > 0,
        "{no_room} {silences} {woken}"
    );
}

/// The same seed gives the same pieces, ends and waits: one trace.
#[test]
fn the_same_seed_gives_the_same_trace() {
    let run = |seed: u64| {
        let faults = Faults::NONE
            .with_partial_recv(300_000)
            .with_copied(100_000)
            .with_ring_full(100_000);
        let (mut io, mut out) = connected(LIMITS, seed, faults);
        const SENT: u32 = 60 * 251;
        send(&mut io, SENT);
        let (mut at, mut left, mut ends) = (0, SENT, 0u32);
        while left > 0 && ends < 400 {
            let (got, _ended) = drain(&mut io, &mut out, left, &mut at);
            (left, ends) = (left - got, ends + 1);
        }
        (io.trace_hash(), at, ends)
    };
    for seed in 0..20 {
        assert_eq!(run(seed), run(seed), "seed {seed}");
    }
    assert_ne!(run(1).0, run(2).0);
}
