// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A connection that sends every frame as a send of its own
//! (`ConnOpts::with_frame_on_packet`): each frame begins a packet, none
//! is put behind another in one send, the next follows when the last was
//! taken and not when its memory is back, and what is queued behind a
//! frame part-sent, a close or a shutdown is treated as ever. The
//! simulator makes every send a packet, so a packet's start is a send's.

use super::*;

/// Sizes whose segments hold one frame of these tests each.
fn roomy(connects: u32, accepts: u32, listeners: u32) -> Sizes {
    sizes(connects, accepts, listeners)
        .with_segments(16, 4096)
        .with_spans(8)
}

fn world(seed: u64, faults: Faults) -> World {
    World::with(seed, faults, roomy(4, 0, 0), roomy(2, 4, 2))
}

/// Where in the client's stream the packets sent so far began: the
/// lengths of all of them, summed up. Only the client sends here.
fn packet_starts(w: &mut World) -> Vec<u64> {
    let mut lens = [0u32; 256];
    let (n, ever) = w.io.last_packets(&mut lens);
    assert_eq!(n as u64, ever, "more packets than the simulator keeps");
    let mut at = 0;
    lens[..n]
        .iter()
        .map(|len| {
            at += u64::from(*len);
            at - u64::from(*len)
        })
        .collect()
}

/// Send `frames` from the client, `ahead` of them queued before each
/// turn, and collect what the server receives; how many turns it took.
fn deliver(w: &mut World, c: Conn, s: Conn, frames: &[Vec<u8>], ahead: usize) -> u32 {
    let (mut next, mut got, mut turns) = (0, Vec::new(), 0);
    while got.len() < frames.len() {
        let mut queued = 0;
        while next < frames.len() && queued < ahead && send(w, c, &frames[next]) {
            (next, queued) = (next + 1, queued + 1);
        }
        w.turn();
        turns += 1;
        while w.client.next_event().is_some() {}
        while w.server.next_event().is_some() {}
        for (f, bytes) in w.pull_all(s) {
            got.push(bytes);
            w.server.release(f);
        }
        assert!(turns < 50_000, "{} of {} frames", got.len(), frames.len());
    }
    assert!(got == frames, "other frames than were sent");
    turns
}

/// Whether every frame of `frames`, sent back to back, began a packet.
fn each_began_a_packet(w: &mut World, frames: &[Vec<u8>]) -> bool {
    let starts = packet_starts(w);
    let mut at = 0u64;
    frames.iter().all(|f| {
        let began = starts.contains(&at);
        at += f.len() as u64;
        began
    })
}

fn frames(count: u8, len: impl Fn(u8) -> usize) -> Vec<Vec<u8>> {
    (0..count).map(|i| prefixed(len(i), i)).collect()
}

/// Four frames queued at once: with the option each is a send of its
/// own, of its own length; without it they share a segment and a send.
#[test]
fn a_frame_is_a_send_of_its_own_and_is_not_put_behind_another() {
    let sent = frames(4, |i| 100 + 50 * usize::from(i));
    let lens: Vec<u32> = sent.iter().map(|f| f.len() as u32).collect();
    for alone in [true, false] {
        let mut w = world(1, Faults::NONE);
        let (c, s) = w.pair(prefix().with_frame_on_packet(alone));
        let _turns: u32 = deliver(&mut w, c, s, &sent, 4);
        let mut got = [0u32; 256];
        let (n, _) = w.io.last_packets(&mut got);
        if alone {
            assert_eq!(&got[..n], &lens[..], "a send a frame");
            assert!(each_began_a_packet(&mut w, &sent));
        } else {
            assert_eq!(&got[..n], &[lens.iter().sum::<u32>()], "one send for all");
            assert!(!each_began_a_packet(&mut w, &sent));
        }
    }
}

/// Sends that go out short, and frames queued while the one before is
/// part-sent: the rest of a frame goes first, and every frame still
/// begins a packet.
#[test]
fn a_frame_begins_a_packet_though_sends_go_out_short() {
    let (mut short, mut behind) = (0, 0);
    for seed in 0..40 {
        let faults = Faults::NONE.with_short_send(400_000);
        let mut w = world(seed, faults);
        let (c, s) = w.pair(prefix().with_frame_on_packet(true));
        let sent = frames(12, |i| 200 + 311 * usize::from(i % 9));
        // One more frame a turn than goes out: frames wait behind others.
        let _turns: u32 = deliver(&mut w, c, s, &sent, 2);
        assert!(each_began_a_packet(&mut w, &sent), "seed {seed}");
        let starts = packet_starts(&mut w);
        short += u32::from(starts.len() > sent.len());
        behind += u32::from(w.sends > 12);
    }
    assert!(
        short > 30 && behind > 30,
        "{short} {behind}: the faults did not bite"
    );
}

/// With zero-copy sends, whose memory the simulator lets go of one to
/// six turns after it took their bytes, the next frame follows when the
/// last was taken: a frame a turn or nearly, not a frame each time
/// memory returns.
#[test]
fn the_next_frame_follows_when_the_last_was_taken_not_when_it_is_let_go() {
    let mut turns = 0;
    for seed in 0..20 {
        let lend = |s: Sizes| s.with_zero_copy_min(256).with_zero_copy_sends(4, 64);
        let (client, server) = (lend(roomy(4, 0, 0)), lend(roomy(2, 4, 2)));
        let mut w = World::with(seed, Faults::NONE, client, server);
        let (c, s) = w.pair(prefix().with_frame_on_packet(true));
        let sent = frames(40, |i| 600 + 97 * usize::from(i % 5));
        turns += deliver(&mut w, c, s, &sent, 8);
        assert!(each_began_a_packet(&mut w, &sent), "seed {seed}");
    }
    // Eight hundred frames: waiting for each one's memory would take
    // some three turns a frame.
    assert!(turns < 800 * 3 / 2, "{turns} turns for 800 frames");
}

/// A close with frames waiting their turn lets go of all of them.
#[test]
fn frames_waiting_their_turn_are_let_go_of_by_a_close() {
    let sent = frames(6, |i| 300 + 100 * usize::from(i));
    let mut w = world(3, Faults::NONE);
    let (c, _s) = w.pair(prefix().with_frame_on_packet(true));
    for f in &sent {
        assert!(send(&mut w, c, f));
    }
    w.client.close(&mut w.io, c).unwrap();
    for _ in 0..2000 {
        w.turn();
        while w.client.next_event().is_some() {}
        while w.server.next_event().is_some() {}
    }
    let (free, all) = w.client.usage().segments();
    assert_eq!(
        (free, w.client.usage().connections()),
        (all, 0),
        "after the close"
    );
}

/// A shutdown sends the frames waiting their turn first, each as a send
/// of its own, and then ends the stream.
#[test]
fn frames_waiting_their_turn_are_sent_before_a_shutdown() {
    let sent = frames(6, |i| 300 + 100 * usize::from(i));
    let mut w = world(4, Faults::NONE);
    let (c, s) = w.pair(prefix().with_frame_on_packet(true));
    for f in &sent {
        assert!(send(&mut w, c, f));
    }
    w.client.shutdown_write(&mut w.io, c).unwrap();
    let (mut got, mut ended) = (Vec::new(), false);
    for _ in 0..2000 {
        w.turn();
        while w.client.next_event().is_some() {}
        // What arrived is pulled before the end is: the end retires `s`.
        if !ended {
            for (f, bytes) in w.pull_all(s) {
                got.push(bytes);
                w.server.release(f);
            }
        }
        while let Some(e) = w.server.next_event() {
            ended |= matches!(e, Event::Closed { conn, why: Why::Orderly } if conn == s);
        }
    }
    assert!(
        got == sent && ended,
        "{} of 6 frames, ended: {ended}",
        got.len()
    );
    assert!(each_began_a_packet(&mut w, &sent));
}
