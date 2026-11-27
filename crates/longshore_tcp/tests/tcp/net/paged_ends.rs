// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The end of the stream in the mode, between frames and inside one: what
//! the caller is told is what direct mode tells it, and every piece goes
//! back.

use super::paged::{none_out, read, settle, world, world_with, zc};
use super::*;

/// What the server's caller sees of a connection reading in `rx` to
/// which the client sends `frames`, then `tail` bytes of one more frame,
/// then the end of its stream: the frames' bytes and the events after.
fn ending(seed: u64, rx: Rx, frames: &[Vec<u8>], tail: &[u8]) -> (Vec<Vec<u8>>, Vec<Event>) {
    let mut w = world(seed, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix().with_rx(rx)));
    let (c, s) = w.connect(prefix());
    for f in frames {
        while !send(&mut w, c, f) {
            w.turn();
        }
    }
    if !tail.is_empty() {
        w.client.send_bytes(&mut w.io, c, tail).unwrap();
    }
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let (mut got, mut told) = (Vec::new(), Vec::new());
    for _ in 0..5_000 {
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            if e != Event::Readable(s) {
                told.push(e);
                continue;
            }
            while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
                got.push(read(&mut w, &f).unwrap());
                w.server.release(f);
            }
        }
        if told
            .iter()
            .any(|e| matches!(e, Event::Closed { .. } | Event::Finished(_)))
        {
            break;
        }
    }
    if told.iter().any(|e| matches!(e, Event::Finished(_))) {
        w.server.close(&mut w.io, s).unwrap();
    }
    w.client.close(&mut w.io, c).unwrap_or(());
    for _ in 0..200 {
        w.turn();
        while w.client.next_event().is_some() || w.server.next_event().is_some() {}
    }
    settle(&mut w);
    none_out(&mut w);
    (got, told)
}

#[test]
fn the_end_of_the_stream_between_frames_is_told_as_in_direct_mode() {
    for seed in 0..20 {
        let frames: Vec<Vec<u8>> = (0..3).map(|i| prefixed(4092 + 900 * i, i as u8)).collect();
        let direct = ending(seed, Rx::Direct, &frames, &[]);
        let paged = ending(seed, Rx::ZeroCopy, &frames, &[]);
        assert_eq!(paged.0, frames);
        assert_eq!(paged, direct, "seed {seed}");
    }
}

#[test]
fn the_end_of_the_stream_inside_a_frame_is_told_as_in_direct_mode() {
    for seed in 0..20 {
        let frames: Vec<Vec<u8>> = (0..2u8).map(|i| prefixed(4092, i)).collect();
        // A header and some of the body; and a header cut short.
        for cut in [3000, 2] {
            let tail = &prefixed(6000, 9)[..cut];
            let direct = ending(seed, Rx::Direct, &frames, tail);
            let paged = ending(seed, Rx::ZeroCopy, &frames, tail);
            assert_eq!(paged.0, frames);
            assert_eq!(paged, direct, "seed {seed}, cut at {cut}");
        }
    }
}

/// One turn of a driver that ticks the backend and routes what came, but
/// polls only the client's set.
fn tick_without_server_poll(w: &mut World) {
    let now = w.io.now_ns();
    w.client.poll(&mut w.io, now);
    if !w.ticking {
        w.io.submit(TICK, Op::Timeout { nanos: MS / 4 }).unwrap();
        w.ticking = true;
    }
    w.io.tick(true, &mut w.out).unwrap();
    w.collect();
}

/// Connections that each send a header announcing a frame longer than
/// the framing allows, to a set of one slot that is not polled between
/// them: each ends, giving up the header's piece, and is closed, and the
/// slot takes the next. However many there are, every piece goes back.
#[test]
fn headers_given_up_between_two_polls_go_back_however_many() {
    let one = sizes(0, 1, 1).with_segments(8, 8192).with_zc(64, 8, 256);
    for seed in 0..10 {
        let mut w = world_with(seed, Faults::NONE, one);
        let _: Listener = w.listen(ListenConfig::new(zc()));
        for _ in 0..4 {
            let c = w.client.connect(&mut w.io, ADDR, prefix()).unwrap();
            let (mut up, mut s, mut sent) = (false, None, false);
            for _ in 0..2_000 {
                tick_without_server_poll(&mut w);
                while let Some(e) = w.client.next_event() {
                    up |= e == Event::Up(c);
                }
                if s.is_none() {
                    if let Some(Event::Accepted { conn, .. }) = w.server.next_event() {
                        s = Some(conn);
                    }
                }
                if up && s.is_some() && !sent {
                    w.client.send_bytes(&mut w.io, c, &[0xff; 4]).unwrap();
                    sent = true;
                }
                if s.is_some_and(|s| !w.server.is_up(s)) {
                    break;
                }
            }
            let s = s.unwrap();
            assert!(!w.server.is_up(s), "seed {seed}: the header never came");
            w.server.close(&mut w.io, s).unwrap();
            w.client.close(&mut w.io, c).unwrap();
            for _ in 0..50 {
                tick_without_server_poll(&mut w);
            }
        }
        settle(&mut w);
        while w.client.next_event().is_some() || w.server.next_event().is_some() {}
        settle(&mut w);
        none_out(&mut w);
    }
}
