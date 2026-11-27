// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy sends whose memory the transport reads past their report:
//! the next send follows without waiting for it, frames arrive whole and
//! in order however the memory returns, and nothing that depends on the
//! memory (a segment, frame memory, a slot, a drain) is let go of early.
//! The simulator stops a run in which bytes are written while they are
//! still being sent, so every test here is also a test of that.

use super::*;

/// Sizes whose frames from 256 bytes up go without a copy, with `pins`
/// of a connection's sends allowed to have their memory out.
fn lending(connects: u32, accepts: u32, listeners: u32, pins: u32) -> Sizes {
    sizes(connects, accepts, listeners)
        .with_zero_copy_min(256)
        .with_zero_copy_sends(pins, 64)
}

fn world(seed: u64, pins: u32) -> World {
    let (client, server) = (lending(4, 0, 0, pins), lending(2, 4, 2, pins));
    World::with(seed, Faults::NONE, client, server)
}

/// Whether the client has a segment out though nothing is queued on
/// `conn`: memory that outlives its send.
fn out_with_nothing_queued(w: &mut World, conn: Conn) -> bool {
    let (free, all) = w.client.usage().segments();
    free < all && w.client.send_queue(conn) == 0
}

/// Send `frames` from the client and collect what the server receives;
/// whether memory was ever out with nothing queued.
fn exchange(w: &mut World, c: Conn, s: Conn, frames: &[Vec<u8>]) -> (Vec<Vec<u8>>, bool) {
    let (mut next, mut got, mut outlived) = (0, Vec::new(), false);
    while got.len() < frames.len() {
        while next < frames.len() && send(w, c, &frames[next]) {
            next += 1;
        }
        w.turn();
        outlived |= out_with_nothing_queued(w, c);
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
    (got, outlived)
}

/// Turn until neither side holds a connection, checking on the way that
/// the client never has a segment out without a slot that owns it.
fn until_idle(w: &mut World, what: &str) {
    for _ in 0..20_000 {
        let usage = w.client.usage();
        let (free, all) = usage.segments();
        assert!(
            free == all || usage.connections() > 0,
            "{what}: a segment out and no connection it belongs to"
        );
        if usage.connections() == 0 && w.server.usage().connections() == 0 {
            return;
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            // What is left to pull keeps its connection.
            if let Event::Readable(conn) = e {
                for (f, _) in w.pull_all(conn) {
                    w.server.release(f);
                }
            }
        }
    }
    panic!("{what}: the two sides never came to rest");
}

#[test]
fn frames_arrive_in_order_however_many_sends_have_their_memory_out() {
    for pins in [0, 1, 2, 8] {
        let mut outlived = 0;
        for seed in 0..40 {
            let mut w = world(seed, pins);
            let (c, s) = w.pair(prefix());
            let frames: Vec<Vec<u8>> = (0..30u8)
                .map(|i| prefixed(300 + 97 * usize::from(i % 7), i))
                .collect();
            let (got, seen) = exchange(&mut w, c, s, &frames);
            assert!(got == frames, "{pins} pins, seed {seed}");
            outlived += u32::from(seen);
            w.client.close(&mut w.io, c).unwrap();
            until_idle(&mut w, "after the close");
        }
        // With none, a send keeps its place in the queue until its
        // memory is back; with any, the queue moves on without it.
        assert_eq!(outlived > 20, pins > 0, "{pins} pins: {outlived} of 40");
    }
}

/// Over loopback the kernel copies whatever it is asked, so nothing is
/// lent there: a send's memory is back with its report, as for a copy,
/// from the end that dialled and from the end that accepted at a
/// loopback address.
#[test]
fn nothing_is_lent_over_loopback() {
    let local = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7000);
    for seed in 0..20 {
        let mut w = world(seed, 4);
        let _: Listener = w
            .server
            .listen(&mut w.io, local, ListenConfig::new(prefix()))
            .unwrap();
        let (c, s) = w.connect_to(local, prefix());
        let frames: Vec<Vec<u8>> = (0..30u8)
            .map(|i| prefixed(300 + 97 * usize::from(i % 7), i))
            .collect();
        let (got, outlived) = exchange(&mut w, c, s, &frames);
        assert!(got == frames && !outlived, "dialled, seed {seed}");
        // The other way: the accepting end sends.
        for frame in &frames {
            while w.server.send_bytes(&mut w.io, s, frame).is_err() {
                w.turn();
            }
            w.turn();
            let (free, all) = w.server.usage().segments();
            assert!(
                free == all || w.server.send_queue(s) > 0,
                "accepted, seed {seed}: memory out with nothing queued"
            );
            while w.client.next_event().is_some() {}
            while let Ok(Some(lent)) = w.client.next_frame(&mut w.io, c) {
                w.client.release(lent);
            }
        }
    }
}

#[test]
fn a_frame_sent_from_where_it_lies_keeps_its_memory_until_the_send_lets_go() {
    let opts = prefix().with_rx(Rx::Direct);
    for seed in 0..40 {
        let mut w = world(seed, 2);
        let (c, s) = w.pair(opts);
        let frames: Vec<Vec<u8>> = (0..20u8).map(|i| prefixed(1000, i)).collect();
        let (mut next, mut back) = (0, Vec::new());
        while back.len() < frames.len() {
            while next < frames.len() && send(&mut w, c, &frames[next]) {
                next += 1;
            }
            w.turn();
            while let Some(e) = w.server.next_event() {
                assert!(
                    matches!(e, Event::Readable(_) | Event::Writable(_)),
                    "{e:?}"
                );
            }
            // The server answers each frame with itself, from frame
            // memory, and lets go of it at once: the send's hold is all
            // that keeps the bytes while the next frame is received.
            while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
                assert!(f.is_held(), "direct mode lends from frame memory");
                match w.server.send_lent(s, &f) {
                    Ok(()) => w.server.release(f),
                    Err(e) => panic!("seed {seed}: {e:?}"),
                }
            }
            while let Some(e) = w.client.next_event() {
                assert!(
                    matches!(e, Event::Readable(_) | Event::Writable(_)),
                    "{e:?}"
                );
            }
            while let Some(f) = w.client.next_frame(&mut w.io, c).unwrap() {
                back.push(w.client.bytes(&mut w.io, &f).unwrap().to_vec());
                w.client.release(f);
            }
        }
        assert!(back == frames, "seed {seed}");
        w.client.close(&mut w.io, c).unwrap();
        until_idle(&mut w, "after the close");
        let (free, all) = w.server.usage().frames();
        assert_eq!(free, all, "seed {seed}: frame memory still held");
    }
}

#[test]
fn a_drain_waits_for_the_memory_of_what_was_sent() {
    let mut outlived = 0;
    for seed in 0..60 {
        let mut w = world(seed, 2);
        let (c, _s) = w.pair(prefix());
        assert!(send(&mut w, c, &prefixed(2000, 9)));
        let until = w.io.now_ns() + 1_000 * MS;
        w.client.drain(&mut w.io, until);
        let mut drained = false;
        for _ in 0..20_000 {
            while let Some(e) = w.client.next_event() {
                drained |= e == Event::Drained;
            }
            if drained {
                break;
            }
            outlived += u32::from(out_with_nothing_queued(&mut w, c));
            w.turn();
            while w.server.next_event().is_some() {}
        }
        // Not because the time was up, and with every segment back.
        assert!(drained && w.io.now_ns() < until, "seed {seed}");
        let (free, all) = w.client.usage().segments();
        assert_eq!(free, all, "seed {seed}: drained with memory out");
    }
    assert!(
        outlived > 20,
        "memory outlived its send in {outlived} turns"
    );
}

#[test]
fn a_closed_connection_keeps_its_slot_until_its_memory_is_back() {
    let mut out_after_close = 0;
    for seed in 0..60 {
        let mut w = world(seed, 4);
        let (c, s) = w.pair(prefix());
        for i in 0..4u8 {
            assert!(send(&mut w, c, &prefixed(1500, i)), "seed {seed}");
        }
        // Until the last of them is on its way, then closed at once.
        for _ in 0..200 {
            if w.client.send_queue(c) == 0 {
                break;
            }
            w.turn();
            while w.client.next_event().is_some() {}
            while w.server.next_event().is_some() {}
            for (f, _) in w.pull_all(s) {
                w.server.release(f);
            }
        }
        assert_eq!(w.client.send_queue(c), 0, "seed {seed}");
        w.client.close(&mut w.io, c).unwrap();
        let (free, all) = w.client.usage().segments();
        out_after_close += u32::from(free < all);
        until_idle(&mut w, "a close with memory out");
        // The slot serves the next connection as any other.
        let (again, accepted) = w.connect(prefix());
        let frames = vec![prefixed(700, 1), prefixed(3000, 2)];
        let (got, _) = exchange(&mut w, again, accepted, &frames);
        assert!(got == frames, "seed {seed}");
    }
    assert!(out_after_close > 20, "{out_after_close} of 60 runs");
}

#[test]
fn the_set_bounds_the_memory_out_as_each_connection_does() {
    // Four frames of a segment each, on one connection: with one send's
    // memory allowed out in the whole set, never more than one segment
    // is out once the queue is empty; with room for all, more are.
    for (in_all, most_wanted) in [(1u32, 1u32), (64, 4)] {
        let mut most = 0;
        for seed in 0..60 {
            let client = lending(4, 0, 0, 8).with_zero_copy_sends(8, in_all);
            let mut w = World::with(seed, Faults::NONE, client, lending(2, 4, 2, 8));
            let (c, s) = w.pair(prefix());
            for i in 0..4u8 {
                assert!(send(&mut w, c, &prefixed(3000, i)), "seed {seed}");
            }
            let mut got = 0;
            while got < 4 {
                w.turn();
                while w.client.next_event().is_some() {}
                while w.server.next_event().is_some() {}
                for (f, _) in w.pull_all(s) {
                    got += 1;
                    w.server.release(f);
                }
                let (free, all) = w.client.usage().segments();
                if w.client.send_queue(c) == 0 {
                    most = most.max(all - free);
                }
            }
        }
        assert!(
            most <= most_wanted && (in_all == 1 || most > 1),
            "{in_all} in all: {most} segments out at once"
        );
    }
}

/// One send segment, two connections refused it while a zero-copy send
/// still reads it: the report that its bytes were taken frees a place in
/// the queue but no segment, so neither is told it may send; when the
/// memory is back, one of them is, and its send is accepted. Every
/// `Writable` is one a send can follow.
#[test]
fn a_parked_segment_tells_no_connection_it_may_send() {
    let one = lending(4, 0, 0, 2).with_segments(1, 4096);
    for seed in 0..20 {
        let mut w = World::with(seed, Faults::NONE, one, lending(2, 4, 2, 2));
        let (c1, s1) = w.pair(prefix());
        let (c2, s2) = w.connect(prefix());
        let frame = prefixed(1000, seed as u8);
        assert!(send(&mut w, c1, &frame));
        // Sent with the poll: the segment is the transport's now.
        let now = w.io.now_ns();
        w.client.poll(&mut w.io, now);
        assert!(!send(&mut w, c1, &frame) && !send(&mut w, c2, &frame));
        let (mut waiting, mut got) = (vec![c1, c2], 0);
        for _ in 0..20_000 {
            if waiting.is_empty() && got == 3 {
                break;
            }
            w.turn();
            while let Some(e) = w.client.next_event() {
                let Event::Writable(conn) = e else {
                    panic!("{e:?}");
                };
                assert!(send(&mut w, conn, &frame), "seed {seed}: told in vain");
                waiting.retain(|c| *c != conn);
            }
            while w.server.next_event().is_some() {
                for s in [s1, s2] {
                    for (f, _) in w.pull_all(s) {
                        w.server.release(f);
                        got += 1;
                    }
                }
            }
        }
        assert!(
            waiting.is_empty() && got == 3,
            "seed {seed}: {waiting:?}, {got}"
        );
    }
}
