// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Input at its edges in ring mode: a header that is a whole frame and
//! arrives in pieces, a stream that ends inside a frame, reading that
//! stopped for want of frame memory, and a backlog with more frames lent
//! from it than can be counted.

use super::*;

/// Turn until the server has events for `conn`, and pull every frame of
/// each `Readable`. Anything but `Readable` for `conn` ends it: that event.
fn pull_until_other(w: &mut World, conn: Conn, got: &mut Vec<Vec<u8>>) -> Event {
    let mut other = None;
    w.until(|w, server, e| {
        match e {
            Event::Readable(readable) if server && readable == conn => {
                got.extend(w.pull_all(conn).into_iter().map(|(_, bytes)| bytes));
            }
            Event::Writable(_) if !server => {}
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => {
                assert!(server, "unexpected {e:?} at the client");
                other = Some(e);
            }
        }
        other.is_some()
    });
    other.unwrap()
}

#[test]
fn a_frame_that_is_only_a_header_arrives_when_its_header_came_in_pieces() {
    let mut w = World::new(3, Faults::NONE);
    let (c, s) = w.pair(prefix());
    // A length prefix of zero, two bytes at a time, and then the end.
    assert!(send(&mut w, c, &[0, 0]));
    for _ in 0..20 {
        w.turn();
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            assert!(w.pull_all(s).is_empty(), "half a header is no frame");
        }
    }
    assert!(send(&mut w, c, &[0, 0]));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let mut got = Vec::new();
    let ended = pull_until_other(&mut w, s, &mut got);
    assert_eq!(got, [prefixed(0, 0)]);
    let orderly = Event::Closed {
        conn: s,
        why: Why::Orderly,
    };
    assert_eq!(ended, orderly);
}

#[test]
fn a_stream_that_ends_inside_a_frame_is_a_broken_connection() {
    let mut w = World::new(3, Faults::NONE);
    let (c, s) = w.pair(prefix());
    // Half of a frame, and then the end of the stream, both there before
    // the server's caller looks.
    let whole = prefixed(100, 4);
    assert!(send(&mut w, c, &whole[..54]));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    for _ in 0..50 {
        w.turn();
    }
    let mut got = Vec::new();
    let ended = pull_until_other(&mut w, s, &mut got);
    assert!(got.is_empty(), "half a frame was delivered");
    let broken = Event::Closed {
        conn: s,
        why: Why::Reset,
    };
    assert_eq!(ended, broken);
}

#[test]
fn reading_stopped_for_want_of_frame_memory_goes_on_once_the_input_is_pulled() {
    let mut w = World::new(9, Faults::NONE);
    let (ca, sa) = w.pair(prefix());
    let (cb, sb) = w.connect(prefix());
    // The caller holds all eight buffers of frame memory.
    for i in 0..8 {
        assert!(send(&mut w, cb, &prefixed(10, i)));
    }
    let mut held = Vec::new();
    while held.len() < 8 {
        w.turn();
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(sb));
            while let Some(f) = w.server.next_frame(&mut w.io, sb).unwrap() {
                held.push(w.server.hold(&mut w.io, f).unwrap());
            }
        }
    }
    // Input the caller leaves in the ring over a `poll` has nowhere to
    // go: the connection keeps its ring buffers and stops reading.
    let first = prefixed(100, 1);
    assert!(send(&mut w, ca, &first));
    for _ in 0..20 {
        w.turn();
    }
    assert_eq!(w.server.next_event(), Some(Event::Readable(sa)));
    let pulled = w.pull_all(sa);
    assert_eq!(pulled.len(), 1);
    assert!(pulled[0].1 == first && !pulled[0].0.is_held());
    for f in held {
        w.server.release(f);
    }
    // Pulled, it reads again.
    let second = prefixed(200, 2);
    assert!(send(&mut w, ca, &second));
    let mut got = Vec::new();
    w.until(|w, server, e| {
        match e {
            Event::Readable(conn) if server && conn == sa => {
                got.extend(w.pull_all(sa).into_iter().map(|(_, bytes)| bytes));
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
        !got.is_empty()
    });
    assert_eq!(got, [second]);
}

/// The number a counted frame carries behind its length prefix.
fn number(w: &mut World, f: &Frame) -> u32 {
    let bytes = w.server.bytes(&mut w.io, f).unwrap();
    u32::from_be_bytes(bytes[4..8].try_into().unwrap())
}

#[test]
fn a_frame_the_backlog_cannot_count_another_hold_for_is_pulled_later() {
    // A megabyte of frame memory: a backlog of more eight-byte frames
    // than one buffer has holds (65,535, one of them the set's own).
    const HOLDS: u32 = 65_534;
    const FRAMES: u32 = HOLDS + 40;
    let client = sizes(4, 0, 0).with_frames(2, 1 << 20);
    let server = sizes(2, 4, 2).with_frames(4, 1 << 20);
    let mut w = World::with(1, Faults::NONE, client, server);
    let (c, s) = w.pair(prefix());
    // The server's caller pulls nothing until everything has arrived.
    let mut next = 0;
    while next < FRAMES || w.client.send_queue(c) > 0 {
        while next < FRAMES {
            let mut frame = 4u32.to_be_bytes().to_vec();
            frame.extend(next.to_be_bytes());
            if !send(&mut w, c, &frame) {
                break;
            }
            next += 1;
        }
        w.turn();
        while w.client.next_event().is_some() {}
    }
    for _ in 0..200 {
        w.turn();
    }
    let mut frames = Vec::new();
    while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
        assert_eq!(number(&mut w, &f), frames.len() as u32);
        frames.push(f);
    }
    assert_eq!(frames.len() as u32, HOLDS, "frames held before the limit");
    // One hold fewer, one frame more: the one that found no hold.
    w.server.release(frames.swap_remove(0));
    w.turn();
    let f = w.server.next_frame(&mut w.io, s).unwrap().unwrap();
    assert_eq!(number(&mut w, &f), HOLDS);
    assert_eq!(w.server.next_frame(&mut w.io, s).unwrap(), None);
}
