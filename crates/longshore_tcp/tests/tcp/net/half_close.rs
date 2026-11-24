// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Half-close: a peer that sends, shuts its write side down and waits is
//! answered, when the connection was opened for that; and what a
//! connection that is not open for sending says.

use super::{prefix, prefixed, send, World, ADDR, MS, SPARE};
use longshore_common::io::Io;
use longshore_common::sim::Faults;
use longshore_tcp::{
    Conn, ConnOpts, Event, Framing, ListenConfig, Listener, NetError, SendError, Why,
};
use std::net::SocketAddr;

/// What the server's caller is told, in order, until `last` (included).
/// Frames are pulled at each `Readable` and kept. The client's events are
/// kept apart.
#[cfg(test)]
struct Told {
    server: Vec<Event>,
    client: Vec<Event>,
    frames: Vec<Vec<u8>>,
}

#[cfg(test)]
fn until_told(w: &mut World, last: Event) -> Told {
    let mut told = Told {
        server: Vec::new(),
        client: Vec::new(),
        frames: Vec::new(),
    };
    for _ in 0..4000 {
        while let Some(e) = w.client.next_event() {
            told.client.push(e);
        }
        while let Some(e) = w.server.next_event() {
            if let Event::Readable(conn) = e {
                let pulled = w.pull_all(conn);
                told.frames
                    .extend(pulled.into_iter().map(|(_, bytes)| bytes));
            }
            told.server.push(e);
            if e == last {
                return told;
            }
        }
        w.turn();
    }
    panic!("never told {last:?}; told {:?}", told.server);
}

/// A listener whose connections are half-close, and one connection to it
/// from a client that is not.
#[cfg(test)]
fn half_closed_pair(w: &mut World, opts: ConnOpts) -> (Conn, Conn) {
    let _: Listener = w.listen(ListenConfig::new(opts.with_half_close(true)));
    w.connect(prefix())
}

#[test]
fn a_peer_that_sends_shuts_down_and_waits_is_answered() {
    let mut w = World::new(31, Faults::NONE);
    let (c, s) = half_closed_pair(&mut w, prefix());
    let (request, answer) = (prefixed(300, 1), prefixed(700, 2));
    assert!(send(&mut w, c, &request));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    // The request, and then word that the peer has finished.
    let told = until_told(&mut w, Event::Finished(s));
    assert_eq!(told.frames, [request]);
    assert_eq!(told.server, [Event::Readable(s), Event::Finished(s)]);
    // The answer goes out after the peer's end; then our side is done too.
    assert!(!w.server.is_up(s) && w.server.can_send(s));
    assert_eq!(w.server.send_bytes(&mut w.io, s, &answer), Ok(()));
    w.server.shutdown_write(&mut w.io, s).unwrap();
    assert!(!w.server.can_send(s));
    let ended = Event::Closed {
        conn: s,
        why: Why::Orderly,
    };
    let rest = until_told(&mut w, ended);
    assert_eq!(rest.server, [ended]);
    // The peer got it, and its connection ended in order as well.
    let got = w.client.next_frame(&mut w.io, c).unwrap().unwrap();
    assert_eq!(w.client.bytes(&mut w.io, &got), Some(&answer[..]));
    w.client.release(got);
    let mut client_end = rest.client;
    for _ in 0..200 {
        w.turn();
        while let Some(e) = w.client.next_event() {
            client_end.push(e);
        }
    }
    let ended_there = Event::Closed {
        conn: c,
        why: Why::Orderly,
    };
    assert_eq!(client_end.last(), Some(&ended_there));
}

#[test]
fn without_half_close_the_peers_end_ends_the_connection() {
    let mut w = World::new(32, Faults::NONE);
    let (c, s) = w.pair(prefix());
    assert!(send(&mut w, c, &prefixed(300, 1)));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    // Everything has arrived, the end of the stream too; nothing pulled.
    for _ in 0..60 {
        w.turn();
    }
    assert_eq!(w.pull_all(s).len(), 1);
    // The last frame is pulled: the connection has ended, and says so.
    assert_eq!(
        w.server.send_bytes(&mut w.io, s, &prefixed(5, 2)),
        Err(SendError::NotUp)
    );
    assert_eq!(w.server.shutdown_write(&mut w.io, s), Err(NetError::NotUp));
    let ended = Event::Closed {
        conn: s,
        why: Why::Orderly,
    };
    let told = until_told(&mut w, ended);
    assert!(!told.server.contains(&Event::Finished(s)));
}

#[test]
fn a_stream_that_ends_inside_a_frame_is_a_reset_and_not_finished() {
    let mut w = World::new(33, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix().with_half_close(true)));
    // The client sends raw bytes: a whole frame, then a header that
    // promises a hundred bytes and ten of them.
    let (c, s) = w.connect(ConnOpts::new(Framing::Bytes));
    let whole = prefixed(20, 1);
    let mut bytes = whole.clone();
    bytes.extend([0, 0, 0, 100]);
    bytes.extend([7; 10]);
    w.client.send_bytes(&mut w.io, c, &bytes).unwrap();
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let ended = Event::Closed {
        conn: s,
        why: Why::Reset,
    };
    let told = until_told(&mut w, ended);
    assert_eq!(told.frames, [whole]);
    assert!(!told.server.contains(&Event::Finished(s)));
}

#[test]
fn a_half_closed_connection_that_is_never_answered_times_out() {
    let mut w = World::new(34, Faults::NONE);
    let (c, s) = half_closed_pair(&mut w, prefix().with_idle_timeout_ns(20 * MS));
    assert!(send(&mut w, c, &prefixed(10, 1)));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let _: Told = until_told(&mut w, Event::Finished(s));
    let since = w.io.now_ns();
    let ended = Event::Closed {
        conn: s,
        why: Why::Timeout,
    };
    let told = until_told(&mut w, ended);
    assert_eq!(told.server, [ended]);
    assert!(w.io.now_ns() - since >= 20 * MS);
}

#[test]
fn a_caller_that_shut_down_first_is_not_told_the_peer_finished() {
    let mut w = World::new(35, Faults::NONE);
    let (c, s) = half_closed_pair(&mut w, prefix());
    w.server.shutdown_write(&mut w.io, s).unwrap();
    assert!(send(&mut w, c, &prefixed(10, 1)));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let ended = Event::Closed {
        conn: s,
        why: Why::Orderly,
    };
    let told = until_told(&mut w, ended);
    assert_eq!(told.frames.len(), 1);
    assert_eq!(told.server, [Event::Readable(s), ended]);
}

#[test]
fn closing_a_half_closed_connection_under_its_answer_leaves_nothing_behind() {
    let mut w = World::new(36, Faults::NONE);
    let (c, s) = half_closed_pair(&mut w, prefix());
    assert!(send(&mut w, c, &prefixed(10, 1)));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let _: Told = until_told(&mut w, Event::Finished(s));
    assert_eq!(
        w.server.send_bytes(&mut w.io, s, &prefixed(3000, 2)),
        Ok(())
    );
    w.server.close(&mut w.io, s).unwrap();
    // Nothing more is said of it, and its slot serves the next peers.
    for _ in 0..100 {
        w.turn();
        while w.client.next_event().is_some() {}
        assert_eq!(w.server.next_event(), None);
    }
    for _ in 0..4 {
        let (again, there) = w.connect(prefix());
        w.client.close(&mut w.io, again).unwrap();
        // The peer's end leaves it half-closed; the caller ends it.
        let _: Told = until_told(&mut w, Event::Finished(there));
        w.server.close(&mut w.io, there).unwrap();
    }
}

#[test]
fn a_send_that_fails_after_the_peer_finished_ends_the_connection_as_reset() {
    let mut w = World::new(37, Faults::NONE);
    let (c, s) = half_closed_pair(&mut w, prefix());
    assert!(send(&mut w, c, &prefixed(10, 1)));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let _: Told = until_told(&mut w, Event::Finished(s));
    // The peer goes away altogether while the answer is on its way.
    w.client.close(&mut w.io, c).unwrap();
    for _ in 0..20 {
        w.turn();
    }
    let ended = Event::Closed {
        conn: s,
        why: Why::Reset,
    };
    for _ in 0..50 {
        // Until one of them is refused by the network.
        if w.server
            .send_bytes(&mut w.io, s, &prefixed(2000, 3))
            .is_err()
        {
            break;
        }
        w.turn();
    }
    let told = until_told(&mut w, ended);
    assert_eq!(told.server.last(), Some(&ended));
}

#[test]
fn a_connection_that_is_not_up_says_so_and_keeps_its_handle() {
    let mut w = World::new(38, Faults::NONE);
    // Something listens on the address and never accepts.
    let _: SocketAddr = w.io.listen(SPARE, ADDR).unwrap();
    let c = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
    assert_eq!(w.client.shutdown_write(&mut w.io, c), Err(NetError::NotUp));
    assert_eq!(
        w.client.send_bytes(&mut w.io, c, &prefixed(3, 1)),
        Err(SendError::NotUp)
    );
    // The handle still names the connection: it can be closed, once.
    assert_eq!(w.client.close(&mut w.io, c), Ok(()));
    assert_eq!(w.client.close(&mut w.io, c), Err(NetError::Stale));
}

#[test]
fn input_left_unpulled_does_not_time_out_while_the_answer_is_being_sent() {
    let mut w = World::new(39, Faults::NONE);
    let (c, s) = half_closed_pair(&mut w, prefix().with_idle_timeout_ns(20 * MS));
    // Two requests and the end of the stream, all there before a pull.
    assert!(send(&mut w, c, &prefixed(10, 1)));
    assert!(send(&mut w, c, &prefixed(10, 2)));
    w.client.shutdown_write(&mut w.io, c).unwrap();
    for _ in 0..40 {
        w.turn();
    }
    // The caller takes the first and answers it at length, leaving the
    // second where it is: its backpressure.
    let first = w.server.next_frame(&mut w.io, s).unwrap().unwrap();
    w.server.release(first);
    let started = w.io.now_ns();
    let mut pieces = 0;
    while w.io.now_ns() - started < 80 * MS {
        // The queue may be full for a turn; the next piece goes then.
        let piece = w.server.send_bytes(&mut w.io, s, &prefixed(500, 3));
        pieces += u32::from(piece.is_ok());
        w.turn();
        while w.client.next_event().is_some() {
            while let Some(f) = w.client.next_frame(&mut w.io, c).unwrap() {
                w.client.release(f);
            }
        }
        while let Some(e) = w.server.next_event() {
            let quiet = matches!(e, Event::Readable(_) | Event::Writable(_));
            assert!(quiet, "ended while it was sending: {e:?}");
        }
    }
    assert!(pieces > 10, "{pieces} pieces of the answer went out");
    // The second request is still there, and then the peer has finished.
    let rest: Vec<Vec<u8>> = w.pull_all(s).into_iter().map(|(_, bytes)| bytes).collect();
    assert_eq!(rest, [prefixed(10, 2)]);
    let told = until_told(&mut w, Event::Finished(s));
    assert_eq!(told.server, [Event::Finished(s)]);
}
