// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a slot and the shared send segments owe their next user: an accept
//! slot filled again while its last connection is still ending, a closed
//! reconnecting connection, a second shutdown after a redial, and a
//! connection waiting for a segment another one holds.

use super::*;
use longshore_tcp::Reconnect;

/// A connection is closed under a send in flight while the next peer
/// arrives. Whether that peer was put into the slot just closed (the
/// system takes the lowest free one, and may accept before it closes).
fn next_peer_after_a_close_under_a_send(seed: u64) -> bool {
    let mut w = World::with(seed, Faults::NONE, sizes(4, 0, 0), sizes(0, 2, 1));
    let (ca, sa) = w.pair(prefix());
    // The client reads nothing: the server's sends to it pile up until
    // one stays in flight.
    let reply = prefixed(2996, 5);
    for _ in 0..400 {
        while w.server.send_bytes(&mut w.io, sa, &reply).is_ok() {}
        w.turn();
    }
    assert!(w.server.send_queue(sa) > 0, "no send was left in flight");
    // The connection is closed under its send, and the next peer arrives.
    w.server.close(&mut w.io, sa).unwrap();
    let cb = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let (mut up, mut accepted) = (false, None);
    w.until(|_, server, e| {
        match e {
            Event::Up(conn) if !server => up |= conn == cb,
            Event::Accepted { conn, .. } if server => accepted = Some(conn),
            // What the first connection had received and queued.
            Event::Readable(conn) | Event::Writable(conn) if conn == ca || conn == sa => {}
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?} (server: {server})"),
        }
        up && accepted.is_some()
    });
    let sb = accepted.unwrap();
    w.client.close(&mut w.io, ca).unwrap();
    // The new connection carries frames both ways and nothing of the old.
    let (ask, answer) = (prefixed(100, 1), prefixed(200, 2));
    assert!(send(&mut w, cb, &ask));
    let (mut asked, mut answered) = (false, false);
    w.until(|w, server, e| {
        match e {
            Event::Readable(conn) if server && conn == sb => {
                for (_, bytes) in w.pull_all(sb) {
                    assert!(bytes == ask);
                    asked = true;
                    w.server.send_bytes(&mut w.io, sb, &answer).unwrap();
                }
            }
            Event::Readable(conn) if !server && conn == cb => {
                while let Some(f) = w.client.next_frame(&mut w.io, cb).unwrap() {
                    assert_eq!(w.client.bytes(&mut w.io, &f), Some(&answer[..]));
                    answered = true;
                }
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
        }
        asked && answered
    });
    // Nothing the old connection had queued is still counted as unsent.
    let until = w.io.now_ns() + 10_000 * MS;
    w.server.drain(&mut w.io, until);
    w.until(|_, server, e| {
        assert_eq!((server, e), (true, Event::Drained));
        true
    });
    assert!(w.io.now_ns() < until, "sends of a closed connection linger");
    sb.slot() == sa.slot()
}

#[test]
fn an_accept_slot_filled_again_while_its_last_connection_ends_starts_clean() {
    let refilled = (0..40)
        .filter(|seed| next_peer_after_a_close_under_a_send(*seed))
        .count();
    assert!(
        refilled > 0,
        "no seed put the next peer into the closed slot"
    );
}

#[test]
fn a_reconnecting_connection_closed_when_it_goes_down_does_not_come_back() {
    let mut w = World::new(6, Faults::NONE);
    let opts = prefix().with_reconnect(Some(Reconnect::new(MS, 8 * MS, 200)));
    let l = w.listen(ListenConfig::new(prefix()));
    let c = w.client.connect(&mut w.io, ADDR, opts).unwrap();
    let mut s = None;
    w.until(|_, _, e| {
        if let Event::Accepted { conn, .. } = e {
            s = Some(conn);
        }
        e == Event::Up(c)
    });
    w.server.close_listener(&mut w.io, l).unwrap();
    w.server.close(&mut w.io, s.unwrap()).unwrap();
    // The caller answers the connection going down by closing it.
    w.until(|w, server, e| {
        assert!(!server, "{e:?}");
        let down = matches!(e, Event::Down { conn, .. } if conn == c);
        if down {
            w.client.close(&mut w.io, c).unwrap();
        }
        down
    });
    // Its peer is back, and is not dialled again.
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    for _ in 0..400 {
        w.turn();
        assert_eq!(w.client.next_event(), None);
        assert_eq!(w.server.next_event(), None);
    }
    assert!(!w.client.is_up(c));
    // Its slot is free: every slot of the set can be used.
    for _ in 0..4 {
        let _: (Conn, Conn) = w.connect(prefix());
    }
}

/// Turn until the server hears that `conn`'s peer ended the stream in
/// order. The client's connection going down and coming up on the way is
/// what a reconnecting connection does.
fn ended_in_order(w: &mut World, conn: Conn) {
    w.until(|_, server, e| match e {
        Event::Closed { conn: ended, why } if server && ended == conn => {
            assert_eq!(why, Why::Orderly);
            true
        }
        Event::Down { .. } | Event::Up(_) if !server => false,
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
}

#[test]
fn a_connection_that_came_up_again_can_shut_its_write_side_down_again() {
    let mut w = World::new(6, Faults::NONE);
    let opts = prefix().with_reconnect(Some(Reconnect::new(MS, 8 * MS, 200)));
    let (c, first) = w.pair(opts);
    w.client.shutdown_write(&mut w.io, c).unwrap();
    ended_in_order(&mut w, first);
    // The server closed its end in turn: down, dialled again, up again.
    let (mut up, mut second) = (false, None);
    w.until(|_, server, e| {
        match e {
            Event::Accepted { conn, .. } if server => second = Some(conn),
            Event::Up(conn) if !server => up |= conn == c,
            Event::Down { conn, .. } if !server => assert_eq!(conn, c),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?} (server: {server})"),
        }
        up && second.is_some()
    });
    w.client.shutdown_write(&mut w.io, c).unwrap();
    ended_in_order(&mut w, second.unwrap());
}

#[test]
fn a_connection_refused_for_want_of_a_segment_is_told_when_one_returns() {
    // Eight segments, four spans to a connection.
    let mut w = World::new(2, Faults::NONE);
    let (c1, _) = w.pair(prefix());
    let (c2, _) = w.connect(prefix());
    let (c3, _) = w.connect(prefix());
    // Frames of 3000 bytes take a 4096-byte segment each.
    let frame = prefixed(2996, 7);
    for conn in [c1, c2] {
        for _ in 0..4 {
            assert!(send(&mut w, conn, &frame));
        }
    }
    // The third connection has nothing queued and finds no segment.
    assert!(!send(&mut w, c3, &frame));
    assert_eq!(w.client.send_queue(c3), 0);
    w.until(|_, server, e| match e {
        Event::Writable(conn) if !server => {
            assert_eq!(conn, c3, "only the third was refused");
            true
        }
        Event::Readable(_) if server => false,
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
    assert!(send(&mut w, c3, &frame), "a segment after Writable");
}

#[test]
fn a_segment_its_waiter_never_took_goes_to_the_next_one_waiting() {
    // One segment for the whole set.
    let client = sizes(4, 0, 0).with_segments(1, 4096);
    let mut w = World::with(2, Faults::NONE, client, sizes(2, 4, 2));
    let (c1, _) = w.pair(prefix());
    let (c2, s2) = w.connect(prefix());
    let (c3, _) = w.connect(prefix());
    let frame = prefixed(2996, 7);
    assert!(send(&mut w, c1, &frame));
    assert!(!send(&mut w, c2, &frame) && !send(&mut w, c3, &frame));
    // The second is told of the segment and is closed without using it.
    let mut told = false;
    w.until(|w, server, e| match e {
        Event::Writable(conn) if !server && !told => {
            assert_eq!(conn, c2, "the one that waited longest");
            w.client.close(&mut w.io, c2).unwrap();
            told = true;
            false
        }
        Event::Writable(conn) if !server => {
            assert_eq!(conn, c3);
            true
        }
        Event::Readable(_) if server => false,
        Event::Closed { conn, .. } if server => {
            assert_eq!(conn, s2);
            false
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
    assert!(send(&mut w, c3, &frame), "a segment after Writable");
}
