// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Slots running out, a port shared by listeners, a bind that has to
//! wait for its address, and accepts that have to wait for a slot.

use super::{prefix, prefixed, send, sizes, World, ADDR, MS};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::{Io, EADDRINUSE};
use longshore_common::sim::Faults;
use longshore_tcp::{Conn, Event, ListenConfig, Listener, NetError, Why};
use std::net::SocketAddr;

/// Turn until the server accepts a connection; the listener and the
/// connection. Connections coming up and ending on the way are ignored.
#[cfg(test)]
fn next_accepted(w: &mut World) -> (Listener, Conn) {
    let mut accepted = None;
    w.until(|_, _, e| {
        match e {
            Event::Accepted { listener, conn } => accepted = Some((listener, conn)),
            Event::Up(_) | Event::Closed { .. } => {}
            Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        accepted.is_some()
    });
    accepted.unwrap()
}

#[test]
fn a_connect_with_every_slot_in_use_is_refused_without_allocating() {
    let mut w = World::new(1, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    for _ in 0..4 {
        let _: (Conn, Conn) = w.connect(prefix());
    }
    let fifth = sealed(|| w.client.connect(&mut w.io, ADDR, prefix()));
    assert_eq!(fifth, Err(NetError::NoSlots));
}

#[test]
fn listeners_that_share_a_port_each_get_whole_connections() {
    let mut w = World::new(4, Faults::NONE);
    let shared = ListenConfig::new(prefix()).with_reuse_port(true);
    let l1 = w.listen(shared);
    let l2 = w.listen(shared);
    let mut conns = Vec::new();
    for _ in 0..4 {
        let _: Conn = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
        let (listener, conn) = next_accepted(&mut w);
        assert!(listener == l1 || listener == l2);
        assert!(!conns.contains(&conn), "a connection was accepted twice");
        conns.push(conn);
    }
}

#[test]
fn a_port_that_is_not_shared_refuses_a_second_listener() {
    let mut w = World::new(4, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let second = w
        .server
        .listen(&mut w.io, ADDR, ListenConfig::new(prefix()));
    assert_eq!(second, Err(NetError::Listen(-EADDRINUSE)));
}

#[test]
fn a_bind_refused_as_in_use_is_retried_until_the_address_is_free() {
    let mut w = World::new(5, Faults::NONE);
    let holder = w.listen(ListenConfig::new(prefix()));
    let waiting = w.listen(ListenConfig::new(prefix()).with_bind_retry_ns(500 * MS));
    assert_eq!(w.server.listener_addr(waiting), None);
    for _ in 0..40 {
        w.turn();
    }
    assert_eq!(w.server.next_event(), None);
    // The holder goes away; the waiting listener takes the address.
    w.server.close_listener(&mut w.io, holder).unwrap();
    for _ in 0..2000 {
        if w.server.listener_addr(waiting).is_some() {
            break;
        }
        w.turn();
    }
    assert_eq!(w.server.listener_addr(waiting), Some(ADDR));
    let _: Conn = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    assert_eq!(next_accepted(&mut w).0, waiting);
}

#[test]
fn a_bind_still_refused_at_its_deadline_is_reported() {
    let mut w = World::new(6, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let waiting = w.listen(ListenConfig::new(prefix()).with_bind_retry_ns(20 * MS));
    let started = w.io.now_ns();
    let mut failed = None;
    w.until(|_, _, e| {
        match e {
            Event::ListenFailed { listener, errno } => failed = Some((listener, errno)),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        true
    });
    assert_eq!(failed, Some((waiting, -EADDRINUSE)));
    assert!(w.io.now_ns() - started >= 20 * MS);
}

#[test]
fn a_listener_slot_with_a_failed_bind_to_report_is_not_given_out_again() {
    let mut w = World::new(6, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let waiting = w.listen(ListenConfig::new(prefix()).with_bind_retry_ns(20 * MS));
    // The retry time runs out while the caller does not look at its events.
    for _ in 0..400 {
        w.turn();
    }
    // It listens elsewhere first. The slot of the failed bind is not free
    // before the failure has been reported: its handle is still the
    // caller's.
    let other = SocketAddr::new(ADDR.ip(), ADDR.port() + 1);
    let early = w
        .server
        .listen(&mut w.io, other, ListenConfig::new(prefix()));
    assert_eq!(early, Err(NetError::NoSlots));
    let failed = Event::ListenFailed {
        listener: waiting,
        errno: -EADDRINUSE,
    };
    assert_eq!(w.server.next_event(), Some(failed));
    assert_eq!(w.server.next_event(), None);
    let next = w
        .server
        .listen(&mut w.io, other, ListenConfig::new(prefix()))
        .unwrap();
    assert_ne!(next, waiting);
    assert_eq!(w.server.listener_addr(next), Some(other));
    // The old handle names nothing, least of all the new listener.
    assert_eq!(w.server.listener_addr(waiting), None);
    let stale = w.server.close_listener(&mut w.io, waiting);
    assert_eq!(stale, Err(NetError::Stale));
    assert_eq!(w.server.listener_addr(next), Some(other));
}

#[test]
fn accepts_wait_after_a_failure_and_go_on_when_a_slot_is_free() {
    // One slot for accepted connections.
    let mut w = World::with(7, Faults::NONE, sizes(4, 0, 0), sizes(0, 1, 1));
    let (_, first) = w.pair(prefix());
    // A second peer connects. The system takes the connection, finds no
    // slot to put it in and drops it; the set hears of a failed accept.
    let second = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let mut dropped = false;
    for _ in 0..200 {
        w.turn();
        while let Some(e) = w.client.next_event() {
            match e {
                Event::Up(conn) => assert_eq!(conn, second),
                Event::Closed { conn, why } => {
                    assert_eq!((conn, why), (second, Why::Reset));
                    dropped = true;
                }
                Event::Accepted { .. }
                | Event::Refused { .. }
                | Event::Readable(_)
                | Event::Writable(_)
                | Event::Down { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => panic!("unexpected {e:?}"),
            }
        }
        // Nothing of it reaches the set's caller.
        assert_eq!(w.server.next_event(), None);
    }
    assert!(dropped);
    // The first connection goes. Once the wait after the failure is over
    // the set accepts again: the next peer gets the slot.
    w.server.close(&mut w.io, first).unwrap();
    let third = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let (_, accepted) = next_accepted(&mut w);
    assert!(w.server.is_up(accepted) && w.client.is_up(third));
}

/// Turn until `wanted` happens; anything else but a queue having room
/// again fails the test.
#[cfg(test)]
fn wait_for(w: &mut World, at_server: bool, wanted: Event) {
    w.until(|_, server, e| match e {
        _ if server == at_server && e == wanted => true,
        Event::Writable(_) => false,
        Event::Up(_)
        | Event::Accepted { .. }
        | Event::Refused { .. }
        | Event::Readable(_)
        | Event::Down { .. }
        | Event::Closed { .. }
        | Event::ListenFailed { .. }
        | Event::Drained => panic!("unexpected {e:?}, waiting for {wanted:?}"),
    });
}

#[test]
fn connections_open_carry_frames_both_ways_and_close_without_allocating() {
    let mut w = World::new(9, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let frame = prefixed(300, 7);
    sealed(|| {
        for _ in 0..20 {
            let (c, s) = w.connect(prefix());
            assert!(send(&mut w, c, &frame));
            // The server sends back what it received, from where it lies.
            wait_for(&mut w, true, Event::Readable(s));
            let got = w.server.next_frame(&mut w.io, s).unwrap().unwrap();
            let held = w.server.hold(&mut w.io, got).unwrap();
            w.server.send_lent(s, &held).unwrap();
            w.server.release(held);
            wait_for(&mut w, false, Event::Readable(c));
            let back = w.client.next_frame(&mut w.io, c).unwrap().unwrap();
            assert_eq!(w.client.bytes(&mut w.io, &back), Some(&frame[..]));
            w.client.release(back);
            // Closed here; the server hears of the end and frees its slot.
            w.client.close(&mut w.io, c).unwrap();
            let ended = Event::Closed {
                conn: s,
                why: Why::Orderly,
            };
            wait_for(&mut w, true, ended);
        }
    });
}
