// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) of the connection set: frames survive any
//! framing and any way the stream is cut into pieces, in both receive
//! modes; raw bytes arrive as they were sent; frames queued together
//! leave in order with their boundaries; and frame memory's hold counts.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_common::io::extents::{Extent, ExtentError, Extents};
use longshore_common::io::{BufId, Limits};
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::check::{fail, must, prefixed, sizes, World};
use longshore_tcp::{Addr, Conn, ConnOpts, Framing, HeaderFraming, ListenConfig, Listener, Rx};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

fn addr() -> Addr {
    Addr::Ip(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)),
        7000,
    ))
}

fn between(tc: &TestCase, low: u32, high: u32) -> u32 {
    tc.draw(gs::integers::<u32>().min_value(low).max_value(high))
}

/// A world of one connection: the server's end has `server`'s options,
/// the client's `client`'s.
fn world(seed: u64, client: ConnOpts, server: ConnOpts) -> (World<SimIo>, Conn, Conn) {
    let mut w = World::new(
        &mut |l| SimIo::new(l, seed, Faults::NONE),
        Limits::BASE,
        sizes(1, 0, 0),
        sizes(0, 1, 1),
    );
    let _: Listener = w.listen(addr(), ListenConfig::new(server));
    let (c, s) = w.connect(addr(), client);
    (w, c, s)
}

/// Send `bytes` from the client, turning while there is no room; whether
/// it had to turn.
fn send(w: &mut World<SimIo>, c: Conn, bytes: &[u8]) -> bool {
    for turned in 0..2000 {
        if w.send(c, bytes) {
            return turned > 0;
        }
        w.turn();
        while w.client().0.next_event().is_some() {}
    }
    fail(format_args!("never room to send {} bytes", bytes.len()))
}

/// Nothing more comes: what was pulled is all there is.
fn nothing_more(w: &mut World<SimIo>, s: Conn, got: &mut Vec<Vec<u8>>) {
    let so_far = got.len();
    pull(w, s, 20, got);
    assert_eq!(got.len(), so_far, "more arrived than was sent");
}

/// Turn `turns` times, pulling and releasing what the server's end has.
fn pull(w: &mut World<SimIo>, s: Conn, turns: u32, got: &mut Vec<Vec<u8>>) {
    for _ in 0..turns {
        w.turn();
        while w.client().0.next_event().is_some() {}
        while w.server().0.next_event().is_some() {}
        for (frame, bytes) in w.pull_all(s) {
            got.push(bytes);
            w.server().0.release(frame);
        }
    }
}

/// A framing with a header of up to twelve bytes and its length field
/// anywhere in it, of any width and either byte order, counting the
/// header or not; and the longest body it allows, up to 1500 bytes.
fn any_framing(tc: &TestCase) -> (HeaderFraming, u32, [u32; 4]) {
    let width = tc.draw(gs::sampled_from(vec![1u32, 2, 4, 8]));
    let header = between(tc, width, 12);
    let at = between(tc, 0, header - width);
    let big = tc.draw(gs::booleans());
    let counts = tc.draw(gs::booleans());
    // What the field can say, as a frame length.
    let field = if width == 1 { 255 } else { 65_535 };
    let most = if counts { field } else { field + header };
    let max_frame = between(tc, header, most.min(header + 1500));
    let framing = HeaderFraming::new(header, at, width as u8, big, counts, max_frame);
    let packed = [header, at, width, u32::from(big) | u32::from(counts) << 1];
    (must(framing, "framing"), max_frame - header, packed)
}

/// A frame for the framing `packed` describes: a header of filler with
/// the length written into its field, and `body` bytes.
fn frame(packed: [u32; 4], body: u32, seed: u8) -> Vec<u8> {
    let [header, at, width, flags] = packed;
    let mut f: Vec<u8> = (0..header + body)
        .map(|i| (i as u8).wrapping_mul(37) ^ seed)
        .collect();
    let said = u64::from(if flags & 2 != 0 { header + body } else { body });
    let field = &mut f[at as usize..(at + width) as usize];
    for (i, b) in field.iter_mut().enumerate() {
        let shift = if flags & 1 != 0 {
            width as usize - 1 - i
        } else {
            i
        };
        *b = (said >> (8 * shift)) as u8;
    }
    f
}

/// Whatever the framing, however the stream is cut on its way, and in
/// either receive mode: the frames that were sent are the frames pulled.
#[hegel::test]
fn frames_survive_any_framing_and_any_cut_of_the_stream(tc: TestCase) {
    let (framing, body_max, packed) = any_framing(&tc);
    let rx = if tc.draw(gs::booleans()) {
        Rx::Ring
    } else {
        Rx::Direct
    };
    let count = between(&tc, 1, 6);
    let frames: Vec<Vec<u8>> = (0..count)
        .map(|i| frame(packed, between(&tc, 0, body_max.min(700)), i as u8))
        .collect();
    let stream: Vec<u8> = frames.concat();
    let server = ConnOpts::new(Framing::Header(framing)).with_rx(rx);
    let (mut w, c, s) = world(u64::from(count), ConnOpts::new(Framing::Bytes), server);
    let (mut at, mut got) = (0, Vec::new());
    while at < stream.len() {
        let piece = (between(&tc, 1, 400) as usize).min(stream.len() - at);
        let _waited: bool = send(&mut w, c, &stream[at..at + piece]);
        at += piece;
        let turns = between(&tc, 0, 3);
        pull(&mut w, s, turns, &mut got);
    }
    for _ in 0..200 {
        if got.len() == frames.len() {
            break;
        }
        pull(&mut w, s, 1, &mut got);
    }
    nothing_more(&mut w, s, &mut got);
    assert!(
        got == frames,
        "{} of {} frames, {rx:?}",
        got.len(),
        frames.len()
    );
}

/// Without framing a stream arrives as the bytes that were sent, in
/// order, in whatever pieces.
#[hegel::test]
fn raw_bytes_arrive_as_they_were_sent(tc: TestCase) {
    let stream = tc.draw(gs::binary().min_size(1).max_size(6000));
    let rx = if tc.draw(gs::booleans()) {
        Rx::Ring
    } else {
        Rx::Direct
    };
    let raw = ConnOpts::new(Framing::Bytes);
    let (mut w, c, s) = world(7, raw, raw.with_rx(rx));
    let (mut at, mut got) = (0, Vec::new());
    while at < stream.len() {
        let piece = (between(&tc, 1, 900) as usize).min(stream.len() - at);
        let _waited: bool = send(&mut w, c, &stream[at..at + piece]);
        at += piece;
        let turns = between(&tc, 0, 2);
        pull(&mut w, s, turns, &mut got);
    }
    for _ in 0..200 {
        if got.iter().map(Vec::len).sum::<usize>() == stream.len() {
            break;
        }
        pull(&mut w, s, 1, &mut got);
    }
    nothing_more(&mut w, s, &mut got);
    assert!(got.concat() == stream, "{} pieces", got.len());
}

/// Frames queued in one turn share segments and leave in fewer sends:
/// they arrive in the order queued, each with its own boundaries. And
/// they do share: where three frames that fit one segment are queued with
/// no turn between, there are fewer sends than frames.
#[hegel::test]
fn frames_queued_together_keep_their_order_and_their_boundaries(tc: TestCase) {
    let framing = must(HeaderFraming::length_prefix(2000), "framing");
    let opts = ConnOpts::new(Framing::Header(framing));
    let (mut w, c, s) = world(9, opts, opts);
    *w.sends() = 0;
    let count = between(&tc, 1, 20);
    let (mut frames, mut got) = (Vec::new(), Vec::new());
    // The frames queued since the last turn, and their bytes; and whether
    // three that fit a segment of 4096 bytes were ever queued so.
    let (mut together, mut bytes, mut shared) = (0, 0, false);
    for i in 0..count {
        let f = prefixed(between(&tc, 0, 1500) as usize, i as u8);
        if send(&mut w, c, &f) {
            (together, bytes) = (0, 0);
        }
        (together, bytes) = (together + 1, bytes + f.len());
        shared |= together >= 3 && bytes <= 4096;
        frames.push(f);
        // Mostly no turn between two frames: they are queued together.
        if between(&tc, 0, 3) == 0 {
            pull(&mut w, s, 1, &mut got);
            (together, bytes) = (0, 0);
        }
    }
    for _ in 0..400 {
        if got.len() == frames.len() {
            break;
        }
        pull(&mut w, s, 1, &mut got);
    }
    nothing_more(&mut w, s, &mut got);
    assert!(got == frames, "{} of {} frames", got.len(), frames.len());
    let sends = *w.sends() as usize;
    assert!(
        !shared || sends < frames.len(),
        "{sends} sends for {count} frames"
    );
}

/// What the test knows of a buffer: its holds, and how often it has
/// been freed.
#[derive(Clone, Copy, Default)]
struct Known {
    holds: u32,
    freed: u32,
}

/// Any takes, holds and releases, with handles of any age: a hold count
/// never goes below zero, a buffer is handed out again only at zero, and
/// a handle from before the buffer was freed is refused.
#[hegel::test]
fn hold_counts_never_go_below_zero_and_a_buffer_is_reused_only_at_zero(tc: TestCase) {
    let count = between(&tc, 1, 5);
    let mut extents = Extents::new(BufId::new(40), count);
    let mut known = vec![Known::default(); count as usize];
    // Every handle ever given out, with how often its buffer had been
    // freed then.
    let mut handles: Vec<(Extent, u32)> = Vec::new();
    for _ in 0..between(&tc, 1, 80) {
        let op = between(&tc, 0, 2);
        if op == 0 || handles.is_empty() {
            let free = known.iter().filter(|k| k.holds == 0).count();
            match extents.take() {
                Some(ext) => {
                    let k = &mut known[(ext.buf().get() - 40) as usize];
                    assert_eq!(k.holds, 0, "a held buffer was handed out");
                    k.holds = 1;
                    handles.push((ext, k.freed));
                }
                None => assert_eq!(free, 0, "none handed out with {free} free"),
            }
        } else {
            let (ext, age) = handles[between(&tc, 0, handles.len() as u32 - 1) as usize];
            let k = &mut known[(ext.buf().get() - 40) as usize];
            let live = age == k.freed && k.holds > 0;
            assert_eq!(extents.holds(ext), live.then_some(k.holds as u16));
            if op == 1 {
                let held = extents.hold(ext);
                assert_eq!(
                    held,
                    if live {
                        Ok(())
                    } else {
                        Err(ExtentError::Stale)
                    }
                );
                k.holds += u32::from(live);
            } else {
                let last = live && k.holds == 1;
                let released = extents.release(ext);
                let want = if live {
                    Ok(last)
                } else {
                    Err(ExtentError::Stale)
                };
                assert_eq!(released, want);
                k.holds -= u32::from(live);
                k.freed += u32::from(last);
            }
        }
        let free = known.iter().filter(|k| k.holds == 0).count();
        assert_eq!(extents.free_count(), free);
    }
}
