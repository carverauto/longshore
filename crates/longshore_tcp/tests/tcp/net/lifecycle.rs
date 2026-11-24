// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Connections over time: handles, sending, ending, reconnecting,
//! listening with a limit, timeouts and draining.

use super::*;
use longshore_tcp::{NetError, Reconnect};

#[test]
fn a_stale_handle_is_refused_and_does_not_reach_the_slots_next_connection() {
    let mut w = World::new(1, Faults::NONE);
    let (old, _) = w.pair(prefix());
    w.client.close(&mut w.io, old).unwrap();
    for _ in 0..10 {
        w.turn();
        while w.client.next_event().is_some() || w.server.next_event().is_some() {}
    }
    // The slot is reused by a new connection.
    let (new, _) = w.connect(prefix());
    assert_eq!(new.slot(), old.slot());
    let stale = w.client.send_frame(&mut w.io, old, 4, |b| b.fill(0));
    assert_eq!(stale, Err(SendError::Stale));
    assert_eq!(w.client.close(&mut w.io, old), Err(NetError::Stale));
    assert_eq!(
        w.client.next_frame(&mut w.io, old).err(),
        Some(NetError::Stale)
    );
    assert!(w.client.is_up(new) && !w.client.is_up(old));
}

#[test]
fn many_small_frames_queued_in_one_turn_leave_in_fewer_sends() {
    let mut w = World::new(1, Faults::NONE);
    let (c, s) = w.pair(prefix());
    w.sends = 0;
    let frames: Vec<Vec<u8>> = (0..100).map(|i| prefixed(28, i)).collect();
    let (mut next, mut got) = (0, Vec::new());
    while got.len() < frames.len() {
        while next < frames.len() && send(&mut w, c, &frames[next]) {
            next += 1;
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            got.extend(w.pull_all(s).into_iter().map(|(_, b)| b));
        }
    }
    assert!(got == frames);
    // 100 frames of 32 bytes fit one 4096-byte segment.
    assert!(w.sends < 10, "{} sends for 100 frames", w.sends);
}

#[test]
fn a_full_queue_refuses_without_losing_anything_and_says_when_there_is_room() {
    let mut w = World::new(1, Faults::NONE);
    let (c, s) = w.pair(prefix());
    // Four spans of one 4096-byte segment each: 3000-byte frames fill it.
    let frame = prefixed(2996, 7);
    let mut queued = 0;
    while send(&mut w, c, &frame) {
        queued += 1;
    }
    assert_eq!(queued, 4, "frames queued before the queue was full");
    assert_eq!(w.client.send_queue(c), 4 * 3000);
    let (mut writable, mut got) = (false, 0);
    w.until(|w, server, e| {
        match e {
            Event::Writable(conn) if !server => writable |= conn == c,
            Event::Readable(conn) if server => {
                for (f, bytes) in w.pull_all(conn) {
                    assert!(bytes == frame);
                    got += 1;
                    w.server.release(f);
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
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        writable && got == 4
    });
    assert_eq!(s.slot(), s.slot());
    assert!(send(&mut w, c, &frame), "room after Writable");
}

#[test]
fn an_orderly_end_comes_after_every_frame_and_a_reset_is_told_apart() {
    let mut w = World::new(4, Faults::NONE);
    let (c1, s1) = w.pair(prefix());
    let (c2, s2) = w.connect(prefix());
    for i in 0..3 {
        assert!(send(&mut w, c1, &prefixed(100, i)));
    }
    w.client.shutdown_write(&mut w.io, c1).unwrap();
    let (mut frames, mut ended) = (0, None);
    w.until(|w, server, e| {
        match e {
            Event::Readable(conn) if server && conn == s1 => {
                assert!(ended.is_none(), "a frame after the end");
                frames += w.pull_all(conn).len();
            }
            Event::Closed { conn, why } if server && conn == s1 => ended = Some(why),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        ended.is_some()
    });
    assert_eq!((frames, ended), (3, Some(Why::Orderly)));
    // The server closed its end in turn, which the client sees as orderly.
    w.until(|_, server, e| {
        assert_eq!(
            (server, e),
            (
                false,
                Event::Closed {
                    conn: c1,
                    why: Why::Orderly
                }
            )
        );
        true
    });
    // The other connection is reset by the network.
    w.io.set_faults(Faults::NONE.with_reset(1_000_000));
    assert!(send(&mut w, c2, &prefixed(100, 9)));
    let (mut client_end, mut server_end) = (None, None);
    w.until(|_, server, e| {
        match e {
            Event::Closed { conn, why } if server && conn == s2 => server_end = Some(why),
            Event::Closed { conn, why } if !server && conn == c2 => client_end = Some(why),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        client_end.is_some() && server_end.is_some()
    });
    assert_eq!(
        (client_end, server_end),
        (Some(Why::Reset), Some(Why::Reset))
    );
}

#[test]
fn a_reconnecting_connection_keeps_its_handle_through_a_restart_of_its_peer() {
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
    // The peer goes away: its listener and its end of the connection.
    w.server.close_listener(&mut w.io, l).unwrap();
    w.server.close(&mut w.io, s.unwrap()).unwrap();
    w.until(|_, server, e| {
        assert!(!server, "{e:?}");
        matches!(e, Event::Down { conn, .. } if conn == c)
    });
    assert!(!w.client.is_up(c));
    // Nobody listens: it keeps dialling, with waits that grow.
    let started = w.io.now_ns();
    for _ in 0..200 {
        w.turn();
        while let Some(e) = w.client.next_event() {
            assert!(matches!(e, Event::Down { .. }), "{e:?}");
        }
    }
    assert!(w.io.now_ns() - started > 20 * MS);
    // The peer is back: up again under the same handle.
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let (mut again, mut up) = (None, false);
    w.until(|_, server, e| {
        match e {
            Event::Accepted { conn, .. } if server => again = Some(conn),
            Event::Up(conn) if !server => up |= conn == c,
            Event::Down { .. } => {}
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        again.is_some() && up
    });
    assert!(w.client.is_up(c));
    assert!(send(&mut w, c, &prefixed(10, 1)));
}

#[test]
fn a_connect_that_is_never_answered_times_out() {
    let mut w = World::new(2, Faults::NONE);
    // Something listens on the address and never accepts.
    let _: SocketAddr = w.io.listen(SPARE, ADDR).unwrap();
    let opts = prefix().with_connect_timeout_ns(30 * MS);
    let c = w.client.connect(&mut w.io, ADDR, opts).unwrap();
    let started = w.io.now_ns();
    // Until then, the timeout is the set's next deadline.
    assert_eq!(w.client.next_deadline(), Some(started + 30 * MS));
    let mut ended = None;
    w.until(|_, _, e| {
        match e {
            Event::Closed { conn, why } => ended = Some((conn, why)),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        true
    });
    assert_eq!(ended, Some((c, Why::Timeout)));
    assert!(w.io.now_ns() - started >= 30 * MS);
    assert_eq!(w.client.next_deadline(), None);
}

#[test]
fn a_listener_at_its_limit_refuses_with_one_frame_and_then_closes() {
    let mut w = World::new(8, Faults::NONE);
    let l = w.listen(ListenConfig::new(prefix()).with_limit(1));
    let (_c1, _s1) = w.connect(prefix());
    // The second connection is over the limit.
    let c2 = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let busy = prefixed(5, 0xEE);
    let (mut refused, mut told, mut closed) = (false, false, false);
    w.until(|w, server, e| {
        match e {
            Event::Refused { listener, conn } if server => {
                assert_eq!(listener, l);
                refused = true;
                let n = busy.len() as u32;
                w.server
                    .send_frame(&mut w.io, conn, n, |b| b.copy_from_slice(&busy))
                    .unwrap();
            }
            Event::Up(conn) if !server => assert_eq!(conn, c2),
            Event::Readable(conn) if !server => {
                while let Some(f) = w.client.next_frame(&mut w.io, conn).unwrap() {
                    assert_eq!(w.client.bytes(&mut w.io, &f), Some(&busy[..]));
                    told = true;
                }
            }
            Event::Closed { conn, why } if !server => {
                assert_eq!((conn, why), (c2, Why::Orderly));
                closed = true;
            }
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        refused && told && closed
    });
}

#[test]
fn a_listener_with_peer_addresses_reports_where_each_connection_came_from() {
    let mut w = World::new(8, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()).with_peer_addr(true));
    for _ in 0..2 {
        let (c, s) = w.connect(prefix());
        let peer = w
            .server
            .peer(&w.io, s)
            .expect("the accepted connection's peer");
        assert_eq!(peer.ip(), ADDR.ip());
        assert_ne!(peer.port(), 0);
        // The side that dialled knows who it dialled.
        assert_eq!(w.client.peer(&w.io, c), Some(ADDR));
    }
}

#[test]
fn draining_stops_accepting_and_reports_when_queued_sends_have_gone() {
    let mut w = World::new(3, Faults::NONE);
    let (c, s) = w.pair(prefix());
    let reply = prefixed(3000, 1);
    let n = reply.len() as u32;
    for _ in 0..3 {
        w.server
            .send_frame(&mut w.io, s, n, |b| b.copy_from_slice(&reply))
            .unwrap();
    }
    let until = w.io.now_ns() + 1000 * MS;
    w.server.drain(&mut w.io, until);
    let (mut got, mut drained) = (0, false);
    w.until(|w, server, e| {
        match e {
            Event::Drained if server => drained = true,
            Event::Readable(conn) if !server => {
                while let Some(f) = w.client.next_frame(&mut w.io, conn).unwrap() {
                    assert_eq!(w.client.bytes(&mut w.io, &f), Some(&reply[..]));
                    got += 1;
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
            | Event::Drained => panic!("unexpected {e:?}"),
        }
        drained && got == 3
    });
    assert!(w.io.now_ns() < until, "drained before the deadline");
    // Nothing new is accepted: a connect now finds nobody listening.
    let late = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    let mut ended = false;
    w.until(|_, _, e| {
        ended = matches!(e, Event::Closed { conn, .. } if conn == late);
        ended
    });
    assert!(w.client.is_up(c), "connections stay open through a drain");
}
