// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The end of the stream in the mode, between frames and inside one: what
//! the caller is told is what direct mode tells it, and every piece goes
//! back.

use super::paged::{none_out, read, settle, world};
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
