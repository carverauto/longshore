// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Frames: cut correctly however the bytes arrive, lent in place, held
//! across turns, and read no faster than the caller releases them.

use super::*;

/// Send `frames` from the client and collect what the server receives.
fn exchange(w: &mut World, c: Conn, s: Conn, frames: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let (mut next, mut got) = (0, Vec::new());
    while got.len() < frames.len() {
        while next < frames.len() && send(w, c, &frames[next]) {
            next += 1;
        }
        w.turn();
        while let Some(e) = w.client.next_event() {
            assert!(matches!(e, Event::Writable(_)), "{e:?}");
        }
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            for (f, bytes) in w.pull_all(s) {
                got.push(bytes);
                w.server.release(f);
            }
        }
    }
    got
}

#[test]
fn length_prefixed_frames_arrive_whole_and_in_order_however_they_are_split() {
    // Frames shorter than a header's worth of bytes, about a ring buffer
    // (1024), and several ring buffers long, under partial receives.
    let lens = [0usize, 1, 3, 500, 1019, 1020, 1021, 2500, 4000, 7, 0, 3000];
    for seed in 0..20 {
        let faults = Faults::NONE
            .with_partial_recv(400_000)
            .with_short_send(300_000);
        let mut w = World::new(seed, faults);
        let (c, s) = w.pair(prefix());
        let frames: Vec<Vec<u8>> = lens.iter().map(|l| prefixed(*l, seed as u8)).collect();
        let got = exchange(&mut w, c, s, &frames);
        assert!(got == frames, "seed {seed}");
    }
}

#[test]
fn a_header_that_carries_the_frame_size_is_read_straight_into_frame_memory() {
    // A 256-byte header whose little-endian u32 at offset 40 is the whole
    // frame, received in direct mode.
    let framing = HeaderFraming::new(256, 40, 4, false, true, 8192).unwrap();
    let opts = ConnOpts::new(Framing::Header(framing)).with_rx(Rx::Direct);
    let frame = |size: u32, seed: u8| {
        let mut f: Vec<u8> = (0..size).map(|i| (i as u8) ^ seed).collect();
        f[40..44].copy_from_slice(&size.to_le_bytes());
        f
    };
    for seed in 0..10 {
        let mut w = World::new(seed, Faults::NONE.with_partial_recv(500_000));
        let (c, s) = w.pair(opts);
        // The segments of this world hold 4096 bytes: frames up to that.
        let frames = vec![frame(256, 1), frame(257, 2), frame(4096, 3), frame(1000, 4)];
        let (mut next, mut got) = (0, Vec::new());
        while got.len() < frames.len() {
            while next < frames.len() && send(&mut w, c, &frames[next]) {
                next += 1;
            }
            w.turn();
            while w.client.next_event().is_some() {}
            while let Some(e) = w.server.next_event() {
                assert_eq!(e, Event::Readable(s));
                for (f, bytes) in w.pull_all(s) {
                    // Direct mode delivers every frame in frame memory.
                    assert!(f.is_held() && f.span().off() == 0);
                    got.push(bytes);
                    w.server.release(f);
                }
            }
        }
        assert!(got == frames, "seed {seed}");
    }
}

#[test]
fn a_frame_longer_than_the_maximum_closes_the_connection_as_a_protocol_error() {
    for rx in [Rx::Ring, Rx::Direct] {
        let mut w = World::new(1, Faults::NONE);
        let (c, s) = w.pair(prefix().with_rx(rx));
        // The prefix announces 8189 bytes: one more than fits 8192 with it.
        assert!(send(&mut w, c, &8189u32.to_be_bytes()));
        let mut closed = None;
        w.until(|w, server, e| {
            match e {
                Event::Readable(conn) if server => assert!(w.pull_all(conn).is_empty()),
                Event::Closed { conn, why } if server => closed = Some((conn, why)),
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
            closed.is_some()
        });
        assert_eq!(closed, Some((s, Why::Protocol)), "{rx:?}");
    }
}

#[test]
fn held_frames_keep_their_bytes_across_turns_and_ring_frames_end_at_the_next_poll() {
    let mut w = World::new(3, Faults::NONE);
    let (c, s) = w.pair(prefix());
    let frames: Vec<Vec<u8>> = (0..4).map(|i| prefixed(200, i)).collect();
    for f in &frames {
        assert!(send(&mut w, c, f));
    }
    let mut lent = Vec::new();
    w.until(|w, server, e| {
        if server && e == Event::Readable(s) {
            lent.extend(w.pull_all(s));
        }
        lent.len() == 4
    });
    // All four lay whole in ring buffers: lent in place.
    assert!(lent.iter().all(|(f, _)| !f.is_held()));
    // Two are held, moving them to frame memory; two are left alone.
    let held: Vec<Frame> = lent[..2]
        .iter()
        .map(|(f, _)| w.server.hold(&mut w.io, *f).unwrap())
        .collect();
    for _ in 0..5 {
        w.turn();
    }
    for (i, f) in held.iter().enumerate() {
        assert_eq!(
            w.server.bytes(&mut w.io, f),
            Some(&frames[i][..]),
            "held frame {i}"
        );
    }
    for (f, _) in &lent {
        assert_eq!(
            w.server.bytes(&mut w.io, f),
            None,
            "a ring frame after the next poll"
        );
    }
    // Released, their memory is free again: all eight frames can be taken.
    for f in held {
        w.server.release(f);
        assert_eq!(w.server.bytes(&mut w.io, &f), None);
    }
    assert_eq!(w.server.extents().free_count(), 8);
}

#[test]
fn a_connection_reads_no_further_while_its_caller_holds_its_budget() {
    // Direct mode, at most 1000 bytes held: one 600-byte frame at a time
    // may be outstanding beyond the first.
    let opts = prefix().with_rx(Rx::Direct).with_held_max(1000);
    let mut w = World::new(5, Faults::NONE);
    let (c, s) = w.pair(opts);
    let frames: Vec<Vec<u8>> = (0..6).map(|i| prefixed(596, i)).collect();
    for f in &frames {
        assert!(send(&mut w, c, f));
    }
    let mut holding: Vec<(Frame, Vec<u8>)> = Vec::new();
    for _ in 0..200 {
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            holding.extend(w.pull_all(s));
        }
    }
    // 600 + 600 reaches the budget: the third frame is not read.
    assert_eq!(
        holding.len(),
        2,
        "frames delivered while the budget is held"
    );
    // Releasing lets the rest through, in order.
    let mut got: Vec<Vec<u8>> = Vec::new();
    while got.len() < frames.len() {
        for (f, bytes) in holding.drain(..) {
            got.push(bytes);
            w.server.release(f);
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            holding.extend(w.pull_all(s));
        }
    }
    assert!(got == frames);
}

#[test]
fn one_connections_unread_input_does_not_keep_the_ring_from_others() {
    let mut w = World::new(9, Faults::NONE);
    let (c1, s1) = w.pair(prefix());
    let (c2, s2) = w.connect(prefix());
    // The first connection's input is never pulled; the ring has four
    // buffers of 1024 bytes, and it is sent six buffers' worth.
    for i in 0..6 {
        assert!(send(&mut w, c1, &prefixed(1020, i)));
        w.turn();
        w.turn();
    }
    // The second still gets everything it is sent.
    let frames: Vec<Vec<u8>> = (0..10).map(|i| prefixed(900, 100 + i)).collect();
    let (mut next, mut got) = (0, Vec::new());
    for _ in 0..2000 {
        while next < frames.len() && send(&mut w, c2, &frames[next]) {
            next += 1;
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            if e == Event::Readable(s2) {
                for (f, bytes) in w.pull_all(s2) {
                    got.push(bytes);
                    w.server.release(f);
                }
            } else {
                assert_eq!(e, Event::Readable(s1));
            }
        }
        if got.len() == frames.len() {
            break;
        }
    }
    assert!(
        got == frames,
        "{} of {} frames arrived",
        got.len(),
        frames.len()
    );
    // And the first connection's input is all still there, in order.
    let first: Vec<Vec<u8>> = w.pull_all(s1).into_iter().map(|(_, b)| b).collect();
    assert!(!first.is_empty());
    for (i, bytes) in first.iter().enumerate() {
        assert!(
            *bytes == prefixed(1020, i as u8),
            "frame {i} of the unread connection"
        );
    }
}

#[test]
fn raw_bytes_are_passed_on_as_they_arrive() {
    for rx in [Rx::Ring, Rx::Direct] {
        let mut w = World::new(2, Faults::NONE.with_partial_recv(300_000));
        let (c, s) = w.pair(ConnOpts::new(Framing::Bytes).with_rx(rx));
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let (mut sent, mut got) = (0, Vec::new());
        while got.len() < data.len() {
            while sent < data.len() {
                let piece = &data[sent..(sent + 1000).min(data.len())];
                let r = w.client.send_bytes(&mut w.io, c, piece);
                if r == Err(SendError::Full) {
                    break;
                }
                r.unwrap();
                sent += piece.len();
            }
            w.turn();
            while w.client.next_event().is_some() {}
            while let Some(e) = w.server.next_event() {
                assert_eq!(e, Event::Readable(s));
                for (f, bytes) in w.pull_all(s) {
                    got.extend(bytes);
                    w.server.release(f);
                }
            }
        }
        assert!(got == data, "{rx:?}");
    }
}
