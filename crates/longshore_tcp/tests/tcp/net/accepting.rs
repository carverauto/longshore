// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A listener under pressure: accepts that keep failing wait longer each
//! time, and a close the driver had no room for is sent again.

use super::*;
use longshore_tcp::NetError;

/// Turn until the client hears that `conn`, dropped by the server's
/// system for want of a slot, has ended; the time that was.
fn dropped_at(w: &mut World, conn: Conn) -> u64 {
    w.until(|_, server, e| match e {
        Event::Up(up) if !server && up == conn => false,
        Event::Closed { conn: ended, why } if !server && ended == conn => {
            assert_eq!(why, Why::Reset);
            true
        }
        Event::Up(_)
        | Event::Accepted { .. }
        | Event::Refused { .. }
        | Event::Readable(_)
        | Event::Writable(_)
        | Event::Down { .. }
        | Event::Closed { .. }
        | Event::ListenFailed { .. }
        | Event::Drained => panic!("unexpected {e:?} (server: {server})"),
    });
    w.io.now_ns()
}

#[test]
fn the_wait_after_a_failed_accept_doubles_while_accepts_keep_failing() {
    // One slot for accepted connections, and it stays taken.
    let mut w = World::with(7, Faults::NONE, sizes(4, 0, 0), sizes(0, 1, 1));
    let (_, _) = w.pair(prefix());
    // A peer the system has no slot for: the first failure, a 5 ms wait.
    let second = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let first_failure = dropped_at(&mut w, second);
    while w.io.now_ns() < first_failure + 8 * MS {
        w.turn();
    }
    // Accepting again, the set fails again at once: a 10 ms wait.
    let third = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let second_failure = dropped_at(&mut w, third);
    assert!(second_failure < first_failure + 12 * MS);
    // Until that wait is over nobody accepts, so the next peer is dropped
    // only then.
    let fourth = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let waited = dropped_at(&mut w, fourth) - second_failure;
    assert!(
        (8 * MS..20 * MS).contains(&waited),
        "the second wait was {waited} ns"
    );
}

#[test]
fn a_listener_close_the_driver_had_no_room_for_is_sent_again() {
    let mut w = World::new(5, Faults::NONE);
    let l = w.listen(ListenConfig::new(prefix()));
    // Every operation the backend can hold is in flight.
    let mut fillers = 0;
    while fillers < 10_000 {
        let token = Token::new(TICK.get() + 1 + fillers);
        if w.io.submit(token, Op::Timeout { nanos: MS }).is_err() {
            break;
        }
        fillers += 1;
    }
    assert!(fillers < 10_000, "the backend never filled up");
    w.server.close_listener(&mut w.io, l).unwrap();
    // The backend has room again.
    while fillers > 0 {
        w.io.tick(true, &mut w.out).unwrap();
        while let Some(c) = w.out.pop() {
            if c.token().get() > TICK.get() {
                fillers -= 1;
            } else {
                assert!(w.server.on_completion(&mut w.io, c), "{c:?}");
            }
        }
    }
    // The listener goes, and its address is free for another.
    let mut again = Err(NetError::NoSlots);
    for _ in 0..200 {
        w.turn();
        again = w
            .server
            .listen(&mut w.io, ADDR, ListenConfig::new(prefix()));
        if again.is_ok() {
            break;
        }
    }
    assert!(again.is_ok(), "the address stayed in use: {again:?}");
    let (_, _) = w.connect(prefix());
}
