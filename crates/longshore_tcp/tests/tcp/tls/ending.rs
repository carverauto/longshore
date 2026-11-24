// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The end of a protected stream in ring mode: what was received before
//! it is delivered, and a close alert among it makes the end orderly,
//! whenever frame memory lets the records be opened.

use super::*;

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
