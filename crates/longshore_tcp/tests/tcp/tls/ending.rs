// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The end of a protected stream in ring mode: what was received before
//! it is delivered, and a close alert among it makes the end orderly,
//! whenever frame memory lets the records be opened.

use super::*;
use longshore_common::io::{Completions, Op, Token};

/// Two frames of a whole record each, a close alert and the end of the
/// stream, all there before the server's caller pulls anything: what it
/// then pulls, and how it is told the connection ended.
fn pulled_after_the_end(seed: u64) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, Why) {
    let certs = certs(NAME);
    let mut w = World::new_tls(
        &mut |l| sim(l, seed, Faults::NONE),
        Limits::BASE,
        secure::sizes(2, 0, 0),
        secure::sizes(0, 1, 1),
        &certs,
    );
    let framing = HeaderFraming::length_prefix(32 << 10).unwrap();
    let opts = ConnOpts::new(Framing::Header(framing));
    let (c, s) = w.pair(Addr::Ip(sim_addr()), opts);
    let frames = vec![check::prefixed(16_380, 1), check::prefixed(16_380, 2)];
    for frame in &frames {
        assert!(w.send(c, frame));
    }
    let (client, io) = w.client();
    client.shutdown_write(io, c).unwrap();
    for _ in 0..600 {
        w.turn();
    }
    let (mut got, mut ended) = (Vec::new(), None);
    w.until(|w, server, e| {
        match e {
            Event::Readable(conn) if server && conn == s => {
                got.extend(w.pull_all(s).into_iter().map(|(_, bytes)| bytes));
            }
            Event::Closed { conn, why } if server && conn == s => ended = Some(why),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Finished(_)
            | Event::Drained => panic!("unexpected {e:?} (server: {server})"),
        }
        ended.is_some()
    });
    (frames, got, ended.unwrap())
}

#[test]
fn records_received_before_the_end_are_delivered_and_a_close_alert_ends_in_order() {
    for seed in 0..40 {
        let (sent, got, why) = pulled_after_the_end(seed);
        assert!(got == sent, "seed {seed}: {} of 2 frames", got.len());
        assert_eq!(why, Why::Orderly, "seed {seed}");
    }
}

/// What the server's caller has pulled from a connection and heard of
/// its end.
struct Heard {
    got: Vec<Vec<u8>>,
    ended: Option<Why>,
}

/// One turn, and then the server's events for `s`: every frame of a
/// `Readable` is pulled and released, and the end is noted.
fn turn_and_hear(w: &mut World<SimIo>, s: Conn, heard: &mut Heard) {
    w.turn();
    while w.client().0.next_event().is_some() {}
    while let Some(e) = w.server().0.next_event() {
        match e {
            Event::Readable(conn) if conn == s => {
                for (f, bytes) in w.pull_all(s) {
                    heard.got.push(bytes);
                    w.server().0.release(f);
                }
            }
            Event::Closed { conn, why } if conn == s => heard.ended = Some(why),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Finished(_)
            | Event::Drained => panic!("unexpected {e:?}"),
        }
    }
}

/// A frame, a close alert and the end of the stream wait in the socket of
/// a connection that is not reading; when it reads again all frame memory
/// is in use elsewhere for a while. What its caller pulls and hears.
fn ended_without_frame_memory(seed: u64) -> Heard {
    let certs = certs(NAME);
    let mut w = World::new_tls(
        &mut |l| sim(l, seed, Faults::NONE),
        Limits::BASE,
        secure::sizes(2, 0, 0),
        secure::sizes(0, 1, 1),
        &certs,
    );
    // One held frame stops the connection reading.
    let (c, s) = w.pair(Addr::Ip(sim_addr()), prefix().with_held_max(1));
    assert!(w.send(c, &check::prefixed(10, 1)));
    let mut held = Vec::new();
    while held.is_empty() {
        w.turn();
        if w.server().0.next_event().is_some() {
            held = w.pull_all(s);
        }
    }
    assert!(w.send(c, &check::prefixed(100, 2)));
    let (client, io) = w.client();
    client.shutdown_write(io, c).unwrap();
    let mut heard = Heard {
        got: Vec::new(),
        ended: None,
    };
    for _ in 0..100 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    // It reads again, and finds no frame memory.
    let (server, _) = w.server();
    for (f, _) in held.drain(..) {
        server.release(f);
    }
    let elsewhere: Vec<_> = std::iter::from_fn(|| server.extents().take()).collect();
    for _ in 0..100 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    for ext in elsewhere {
        assert_eq!(w.server().0.extents().release(ext), Ok(true));
    }
    for _ in 0..400 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    heard
}

#[test]
fn an_end_that_finds_no_frame_memory_waits_for_it_and_loses_nothing() {
    for seed in 0..40 {
        let heard = ended_without_frame_memory(seed);
        assert_eq!(heard.got, [check::prefixed(100, 2)], "seed {seed}");
        assert_eq!(heard.ended, Some(Why::Orderly), "seed {seed}");
    }
}

/// A timer of the test's own, so that time passes when nothing else does.
const BEAT: Token = Token::new(1 << 45);

/// One turn of both sets by hand. While `withheld` is given, the server
/// is not told that its sends have ended, so it does not go on to its
/// next receive: what the client sends meanwhile piles up in its socket.
fn turn_by_hand(
    w: &mut World<SimIo>,
    out: &mut Completions,
    mut withheld: Option<&mut Vec<longshore_common::io::Completion>>,
) {
    let now = w.io().now_ns();
    {
        let (client, io) = w.client();
        client.poll(io, now);
    }
    {
        let (server, io) = w.server();
        server.poll(io, now);
        drop(io.submit(BEAT, Op::Timeout { nanos: 250_000 }));
        io.tick(true, out).unwrap();
    }
    while let Some(c) = out.pop() {
        let taken = {
            let (client, io) = w.client();
            c.token() == BEAT || client.on_completion(io, c)
        };
        if taken {
            continue;
        }
        match withheld.as_deref_mut() {
            // Kind 1 of a connection's tokens is its send.
            Some(later) if c.token().get() % 8 == 1 => later.push(c),
            Some(_) | None => {
                let (server, io) = w.server();
                assert!(server.on_completion(io, c), "nobody claimed {c:?}");
            }
        }
    }
}

#[test]
fn a_frame_behind_the_handshake_waits_for_frame_memory_and_is_delivered() {
    let certs = certs(NAME);
    let mut w = world(&certs, 1);
    let addr = Addr::Ip(sim_addr());
    let l = w.listen(addr, ListenConfig::new(prefix()));
    let to = w.dial_addr(l, addr);
    let c = {
        let (client, io) = w.client();
        client.connect_tls(io, to, NAME, prefix()).unwrap()
    };
    // All frame memory is in use elsewhere when the handshake ends.
    let elsewhere: Vec<_> = std::iter::from_fn(|| w.server().0.extents().take()).collect();
    let frame = check::prefixed(100, 5);
    let mut out = Completions::new(64);
    let (mut later, mut sent) = (Vec::new(), false);
    // The client finishes first and sends its frame; the server reads the
    // end of the handshake and the frame in one receive.
    for _ in 0..400 {
        turn_by_hand(&mut w, &mut out, Some(&mut later));
        if let Some(e) = w.client().0.next_event() {
            assert_eq!(e, Event::Up(c));
            assert!(w.send(c, &frame));
            sent = true;
        }
    }
    assert!(sent, "the client never came up");
    let (server, io) = w.server();
    for done in later {
        assert!(server.on_completion(io, done));
    }
    let mut s = None;
    for _ in 0..200 {
        turn_by_hand(&mut w, &mut out, None);
        match w.server().0.next_event() {
            Some(Event::Accepted { conn, .. }) => s = Some(conn),
            Some(e) => panic!("unexpected {e:?} without frame memory"),
            None => {}
        }
    }
    let s = s.expect("the server never accepted");
    for ext in elsewhere {
        assert_eq!(w.server().0.extents().release(ext), Ok(true));
    }
    let mut got = Vec::new();
    for _ in 0..200 {
        turn_by_hand(&mut w, &mut out, None);
        while let Some(e) = w.server().0.next_event() {
            assert_eq!(e, Event::Readable(s));
            got.extend(w.pull_all(s).into_iter().map(|(_, bytes)| bytes));
        }
    }
    assert_eq!(got, [frame]);
}
