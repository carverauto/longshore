// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A receive in pages that a full completion ring left silent is woken
//! (C5) also when the backend has no room for the cancel on the turn
//! that finds the ring full, the cancel then being asked for again on the
//! next; and in every set on the backend, whichever polls first.

use super::paged::{exchange, mixed, none_out, paged, paired, read, settle, world};
use super::*;

/// The first of the tokens of the harness's own timers that fill the
/// backend, outside both sets' ranges and above `TICK`.
const FILL: u64 = 1 << 41;

/// One turn of the owner's loop, with the backend's operations all in
/// use while the server polls when `fill` says so: every submission of
/// that poll is refused.
fn turn_filled(w: &mut World, fill: bool) {
    let now = w.io.now_ns();
    w.client.poll(&mut w.io, now);
    if !w.ticking {
        w.io.submit(TICK, Op::Timeout { nanos: MS / 4 }).unwrap();
        w.ticking = true;
    }
    let mut n = 0;
    let mut room = fill;
    while room {
        let filler = Op::Timeout { nanos: 1 };
        room = w.io.submit(Token::new(FILL + n), filler).is_ok();
        n += 1;
    }
    w.server.poll(&mut w.io, now);
    w.io.tick(true, &mut w.out).unwrap();
    while let Some(c) = w.out.pop() {
        if c.token() == TICK {
            w.ticking = false;
            continue;
        }
        if c.token().get() >= FILL {
            continue;
        }
        let taken = w.client.on_completion(&mut w.io, c) || w.server.on_completion(&mut w.io, c);
        assert!(taken, "a completion nobody claimed: {c:?}");
    }
}

/// Send `frames` and pull them as they come, with the backend full on
/// every other poll of the server; whether all of them arrived.
fn exchange_filled(w: &mut World, (c, s): (Conn, Conn), frames: &[Vec<u8>]) -> bool {
    let (mut next, mut got) = (0, 0);
    for turn in 0..20_000u32 {
        if got == frames.len() {
            return true;
        }
        while next < frames.len() && send(w, c, &frames[next]) {
            next += 1;
        }
        turn_filled(w, turn % 2 == 1);
        while w.client.next_event().is_some() {}
        while w.server.next_event().is_some() {
            while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
                assert_eq!(read(w, &f).as_deref(), Some(&frames[got][..]));
                w.server.release(f);
                got += 1;
            }
        }
    }
    false
}

#[test]
fn a_silent_receive_whose_cancel_found_no_room_is_woken_on_a_later_turn() {
    let faults = Faults::NONE.with_ring_full(200_000);
    for seed in 0..40 {
        let mut w = world(seed, faults);
        let pair = paired(&mut w);
        let frames = mixed(seed as u8);
        assert!(exchange_filled(&mut w, pair, &frames), "seed {seed}");
        // The harness's own timers end before the sets settle.
        for _ in 0..8 {
            turn_filled(&mut w, false);
        }
        settle(&mut w);
        none_out(&mut w);
    }
}

/// Two sets on one backend, both with pages: the client's polls first.
/// A full ring that the client's set saw must still wake the server's
/// silent receives.
#[test]
fn a_full_ring_wakes_the_silent_receives_of_every_set_on_the_backend() {
    let faults = Faults::NONE.with_ring_full(200_000);
    let client = sizes(4, 0, 0).with_segments(8, 8192).with_zc(64, 8, 256);
    for seed in 0..40 {
        let mut w = World::with(seed, faults, client, paged(64, 8, 256));
        let pair = paired(&mut w);
        let _none: Vec<Frame> = exchange(&mut w, pair, &mixed(seed as u8), |_| false);
        settle(&mut w);
        none_out(&mut w);
    }
}
