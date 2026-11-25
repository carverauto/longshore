// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a connection ends: nothing received is lost whatever the caller
//! pulls first, a refused connection gets one frame, and a listener's slot
//! is not taken again while its accept still has something to report.

use super::{prefix, prefixed, send, World, ADDR};
use longshore_common::sim::Faults;
use longshore_tcp::{Conn, Event, ListenConfig, Listener, NetError, SendError, Why};

/// Pull every event of both sets, and nothing else, for `turns` turns;
/// what the server's caller was told.
#[cfg(test)]
fn drain_events_only(w: &mut World, turns: u32) -> Vec<Event> {
    let mut told = Vec::new();
    for _ in 0..turns {
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            told.push(e);
        }
    }
    told
}

#[test]
fn frames_are_not_lost_when_events_are_pulled_before_them() {
    let mut w = World::new(21, Faults::NONE);
    let (c, s) = w.pair(prefix());
    let frames = [prefixed(40, 1), prefixed(900, 2), prefixed(3, 3)];
    for f in &frames {
        assert!(send(&mut w, c, f));
    }
    w.client.shutdown_write(&mut w.io, c).unwrap();
    // The caller drains its events and looks at no frame.
    let told = drain_events_only(&mut w, 60);
    assert!(!told.is_empty());
    for e in told {
        assert_eq!(e, Event::Readable(s), "an end was told before its frames");
    }
    // Everything the peer sent is still there, and only then the end.
    let got: Vec<Vec<u8>> = w.pull_all(s).into_iter().map(|(_, bytes)| bytes).collect();
    assert_eq!(got, frames);
    let after = drain_events_only(&mut w, 10);
    let ended = Event::Closed {
        conn: s,
        why: Why::Orderly,
    };
    assert_eq!(after, [ended]);
}

#[test]
fn a_refused_connection_is_sent_one_frame_and_no_more() {
    let mut w = World::new(22, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()).with_limit(1));
    let _: (Conn, Conn) = w.connect(prefix());
    let _: Conn = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let mut refused = None;
    w.until(|_, server, e| {
        match e {
            Event::Refused { conn, .. } if server => refused = Some(conn),
            Event::Up(_) => {}
            Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Finished(_)
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        refused.is_some()
    });
    let conn = refused.unwrap();
    let busy = prefixed(5, 0xEE);
    assert_eq!(w.server.send_bytes(&mut w.io, conn, &busy), Ok(()));
    assert_eq!(
        w.server.send_bytes(&mut w.io, conn, &busy),
        Err(SendError::NotUp)
    );
}

#[test]
fn a_listener_slot_is_not_taken_again_while_its_accept_can_still_report() {
    let mut w = World::new(23, Faults::NONE);
    let mut known = Vec::new();
    for round in 0..30 {
        let l = w.listen(ListenConfig::new(prefix()));
        known.push(l);
        // A peer is on its way in when the listener is closed, and the
        // next listener is asked for at once.
        let _: Conn = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
        for _ in 0..(round % 4) {
            w.turn();
        }
        w.server.close_listener(&mut w.io, l).unwrap();
        let mut next = None;
        for _ in 0..200 {
            match w
                .server
                .listen(&mut w.io, ADDR, ListenConfig::new(prefix()))
            {
                Ok(l2) => {
                    next = Some(l2);
                    break;
                }
                // Its predecessors are still closing, or still hold the
                // address: asked again next turn.
                Err(_) => w.turn(),
            }
        }
        let l2 = next.expect("no listener slot came free");
        known.push(l2);
        // What the server announces was accepted by a listener the caller
        // was given, though that one may be closed by now.
        for _ in 0..20 {
            w.turn();
            while w.client.next_event().is_some() {}
            while let Some(e) = w.server.next_event() {
                match e {
                    Event::Accepted { listener, conn } => {
                        assert!(known.contains(&listener), "round {round}: {listener:?}");
                        w.server.close(&mut w.io, conn).unwrap();
                    }
                    Event::Up(_)
                    | Event::Refused { .. }
                    | Event::Readable(_)
                    | Event::Writable(_)
                    | Event::Down { .. }
                    | Event::Closed { .. }
                    | Event::ListenFailed { .. }
                    | Event::Finished(_)
                    | Event::Drained => panic!("round {round}: unexpected {e:?}"),
                }
            }
        }
        w.server.close_listener(&mut w.io, l2).unwrap();
        for _ in 0..20 {
            w.turn();
            while w.client.next_event().is_some() {}
            while w.server.next_event().is_some() {}
        }
    }
}

#[test]
fn closing_a_connection_whose_input_was_never_pulled_frees_everything() {
    let mut w = World::new(24, Faults::NONE);
    let (c, s) = w.pair(prefix());
    for i in 0..3 {
        assert!(send(&mut w, c, &prefixed(1500, i)));
    }
    w.client.shutdown_write(&mut w.io, c).unwrap();
    // The stream has ended with its frames unpulled; the caller closes.
    let told = drain_events_only(&mut w, 60);
    assert!(!told.is_empty());
    w.server.close(&mut w.io, s).unwrap();
    assert_eq!(drain_events_only(&mut w, 60), []);
    let extents = w.server.extents();
    assert_eq!(extents.free_count(), extents.count());
    // Its handle names nothing, and the slot serves the next peer.
    assert_eq!(w.server.close(&mut w.io, s), Err(NetError::Stale));
    let _: (Conn, Conn) = w.connect(prefix());
}
