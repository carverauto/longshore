// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Input at its edges in ring mode: a header that is a whole frame and
//! arrives in pieces, a stream that ends inside a frame, reading that
//! stopped for want of frame memory, and a backlog with more frames lent
//! from it than can be counted.

use super::*;
use longshore_common::io::extents::Extent;

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

/// Send `frames` counted frames on `c`, while the server's caller pulls
/// nothing.
fn send_counted(w: &mut World, c: Conn, frames: u32) {
    let mut next = 0;
    while next < frames || w.client.send_queue(c) > 0 {
        while next < frames {
            let mut frame = 4u32.to_be_bytes().to_vec();
            frame.extend(next.to_be_bytes());
            if !send(w, c, &frame) {
                break;
            }
            next += 1;
        }
        w.turn();
        while w.client.next_event().is_some() {}
    }
}

/// Pull, and keep, every frame `s` gives for now: counted from zero.
fn pull_counted(w: &mut World, s: Conn) -> Vec<Frame> {
    let mut frames = Vec::new();
    while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
        assert_eq!(number(w, &f), frames.len() as u32);
        frames.push(f);
    }
    frames
}

#[test]
fn an_end_behind_frames_the_backlog_cannot_count_a_hold_for_waits_for_them() {
    const HOLDS: u32 = 65_534;
    const FRAMES: u32 = HOLDS + 40;
    let client = sizes(4, 0, 0).with_frames(2, 1 << 20);
    let server = sizes(2, 4, 2).with_frames(4, 1 << 20);
    let mut w = World::with(1, Faults::NONE, client, server);
    let (c, s) = w.pair(prefix());
    send_counted(&mut w, c, FRAMES);
    w.client.shutdown_write(&mut w.io, c).unwrap();
    // Everything and the end have arrived before the caller pulls, and it
    // keeps what it pulls.
    for _ in 0..200 {
        w.turn();
    }
    let frames = pull_counted(&mut w, s);
    assert_eq!(frames.len() as u32, HOLDS, "frames held before the limit");
    // The end is not told while frames wait for a hold.
    for _ in 0..50 {
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            assert!(w.pull_all(s).is_empty(), "a frame without a hold");
        }
        w.turn();
    }
    for f in frames {
        w.server.release(f);
    }
    let mut rest = Vec::new();
    let ended = pull_until_other(&mut w, s, &mut rest);
    let numbers: Vec<u32> = rest
        .iter()
        .map(|bytes| u32::from_be_bytes(bytes[4..8].try_into().unwrap()))
        .collect();
    assert_eq!(numbers, (HOLDS..FRAMES).collect::<Vec<u32>>());
    let orderly = Event::Closed {
        conn: s,
        why: Why::Orderly,
    };
    assert_eq!(ended, orderly);
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
    send_counted(&mut w, c, FRAMES);
    for _ in 0..200 {
        w.turn();
    }
    let mut frames = pull_counted(&mut w, s);
    assert_eq!(frames.len() as u32, HOLDS, "frames held before the limit");
    // One hold fewer, one frame more: the one that found no hold.
    w.server.release(frames.swap_remove(0));
    w.turn();
    let f = w.server.next_frame(&mut w.io, s).unwrap().unwrap();
    assert_eq!(number(&mut w, &f), HOLDS);
    assert_eq!(w.server.next_frame(&mut w.io, s).unwrap(), None);
}

#[test]
fn a_connection_closed_with_input_left_unpulled_owes_no_event() {
    let mut w = World::new(4, Faults::NONE);
    let (c, s) = w.pair(prefix());
    // More input than the caller pulls and one buffer of frame memory
    // holds: the connection stops reading.
    for i in 0..6 {
        assert!(send(&mut w, c, &prefixed(2996, i)));
        for _ in 0..40 {
            w.turn();
            while w.client.next_event().is_some() {}
        }
    }
    assert_eq!(w.server.next_event(), Some(Event::Readable(s)));
    assert_eq!(w.server.next_event(), None);
    // One frame is pulled and released, which lets it read again, and
    // then it is closed with the rest unpulled.
    let f = w.server.next_frame(&mut w.io, s).unwrap().unwrap();
    assert!(f.is_held());
    w.server.release(f);
    w.server.close(&mut w.io, s).unwrap();
    for _ in 0..100 {
        assert_eq!(w.server.next_event(), None);
        w.turn();
        while w.client.next_event().is_some() {}
    }
}

/// All of a frame that is only a header has arrived in pieces, and the
/// end of the stream behind it, while all frame memory is in use
/// elsewhere: the frame has nowhere to go. The connection at the server,
/// whose connections have the options `accepted`, and that frame memory.
fn ended_without_frame_memory(w: &mut World, accepted: ConnOpts) -> (Conn, Vec<Extent>) {
    let _: Listener = w.listen(ListenConfig::new(accepted));
    let (c, s) = w.connect(prefix());
    let elsewhere = std::iter::from_fn(|| w.server.extents().take()).collect();
    // A length prefix of zero, two bytes at a time, and then the end.
    assert!(send(w, c, &[0, 0]));
    for _ in 0..20 {
        w.turn();
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            assert!(w.pull_all(s).is_empty(), "half a header is no frame");
        }
    }
    assert!(send(w, c, &[0, 0]));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    (s, elsewhere)
}

#[test]
fn an_end_whose_last_frame_finds_no_frame_memory_waits_for_it_and_loses_nothing() {
    let mut w = World::new(9, Faults::NONE);
    let (s, elsewhere) = ended_without_frame_memory(&mut w, prefix());
    // However often the caller pulls, the end is not told before the frame.
    let mut asked = 0;
    for _ in 0..400 {
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            assert!(w.pull_all(s).is_empty(), "a frame without frame memory");
            asked += 1;
        }
    }
    assert!(asked > 0, "the input was never offered");
    for ext in elsewhere {
        assert_eq!(w.server.extents().release(ext), Ok(true));
    }
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
fn an_end_waiting_for_frame_memory_is_told_at_the_idle_timeout() {
    let mut w = World::new(9, Faults::NONE);
    let idle = prefix().with_idle_timeout_ns(50 * MS);
    // Frame memory that is never given back.
    let (s, elsewhere) = ended_without_frame_memory(&mut w, idle);
    let started = w.io.now_ns();
    let mut got = Vec::new();
    let ended = pull_until_other(&mut w, s, &mut got);
    assert!(got.is_empty(), "a frame without frame memory");
    let timed_out = Event::Closed {
        conn: s,
        why: Why::Timeout,
    };
    assert_eq!(ended, timed_out);
    assert!(w.io.now_ns() - started >= 50 * MS);
    assert_eq!(elsewhere.len(), 8);
}

#[test]
fn a_header_that_is_no_frame_pulled_after_the_end_is_a_protocol_error() {
    // The prefix announces 8189 bytes: one more than fits 8192 with it.
    let bad = 8189u32.to_be_bytes();
    // Whole in the input, and collected from two pieces.
    for first in [0, 2] {
        let mut w = World::new(3, Faults::NONE);
        let (c, s) = w.pair(prefix());
        if first > 0 {
            assert!(send(&mut w, c, &bad[..first]));
            for _ in 0..20 {
                w.turn();
                while let Some(e) = w.server.next_event() {
                    assert_eq!(e, Event::Readable(s));
                    assert!(w.pull_all(s).is_empty(), "half a header is no frame");
                }
            }
        }
        // The rest and the end of the stream are both there before the
        // server's caller looks.
        assert!(send(&mut w, c, &bad[first..]));
        w.client.shutdown_write(&mut w.io, c).unwrap();
        for _ in 0..50 {
            w.turn();
        }
        let mut got = Vec::new();
        let ended = pull_until_other(&mut w, s, &mut got);
        assert!(got.is_empty(), "no frame was sent");
        let broken = Event::Closed {
            conn: s,
            why: Why::Protocol,
        };
        assert_eq!(ended, broken, "{first} bytes of the header came first");
    }
}
