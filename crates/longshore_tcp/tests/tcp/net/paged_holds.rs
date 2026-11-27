// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Holds of a frame in pages: the caller's, which `release` and `hold`
//! drop once and only once, and a file write's, without which no piece
//! of the run goes back; a frame of a closed connection; and a set given
//! back whole.

use super::paged::{none_out, paired, read, settle, world, world_with};
use super::*;
use longshore_tcp::{NetError, Span};

/// Send frames of one page until one arrives in its pages; frames moved
/// on the way are released. The frame, and its bytes.
fn one_in_pages(w: &mut World, (c, s): (Conn, Conn)) -> (Frame, Vec<u8>) {
    for i in 0..200u8 {
        let frame = prefixed(4092, i);
        let got = super::paged::exchange(w, (c, s), core::slice::from_ref(&frame), |_| true);
        let f = got[0];
        if f.run().is_some() {
            return (f, frame);
        }
        w.server.release(f);
    }
    panic!("no frame arrived in its pages");
}

/// The simulator's pieces out.
fn out(w: &mut World) -> usize {
    w.io.pieces_out().0
}

#[test]
fn release_drops_the_callers_hold_once() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let (f, sent) = one_in_pages(&mut w, pair);
        assert_eq!(read(&mut w, &f), Some(sent));
        w.server.release(f);
        assert!(w.server.pages(&f).is_none(), "released");
        // The second release of the same copy does nothing.
        w.server.release(f);
        settle(&mut w);
        none_out(&mut w);
        assert_eq!(w.server.runs().hold(f.run().unwrap()), Err(NetError::Stale));
    }
}

#[test]
fn a_write_holds_the_pages_past_the_callers_release() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let (f, sent) = one_in_pages(&mut w, pair);
        let run = f.run().unwrap();
        let spans: Vec<Span> = w.server.pages(&f).unwrap().collect();
        let write = w.server.runs().hold(run).unwrap();
        w.server.release(f);
        settle(&mut w);
        // The write still reads them: the pieces are out, the bytes stay.
        assert!(out(&mut w) > 0);
        let mut still = Vec::new();
        for span in &spans {
            let (at, n) = (span.off() as usize, span.len() as usize);
            still.extend_from_slice(&w.io.buf(span.buf())[at..at + n]);
        }
        assert_eq!(still, sent);
        // The caller's hold is gone: its release and hold are stale.
        w.server.release(f);
        assert_eq!(w.server.hold(&mut w.io, f), Err(NetError::Stale));
        assert_eq!(w.server.runs().release(write), Ok(true));
        // A second release of the write's hold cannot be written: it was
        // moved into the first.
        settle(&mut w);
        none_out(&mut w);
    }
}

#[test]
fn hold_copies_into_frame_memory_once_and_the_write_keeps_the_pages() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let (f, sent) = one_in_pages(&mut w, pair);
        let run = f.run().unwrap();
        let write = w.server.runs().hold(run).unwrap();
        let copy = w.server.hold(&mut w.io, f).unwrap();
        assert!(copy.extent().is_some() && copy.run().is_none());
        // The run is no longer the caller's: only the write holds it.
        assert!(w.server.pages(&f).is_none());
        assert_eq!(read(&mut w, &copy), Some(sent.clone()));
        // A release after the hold is stale: the write's hold stays.
        w.server.release(f);
        settle(&mut w);
        assert!(out(&mut w) > 0);
        assert_eq!(w.server.runs().release(write), Ok(true));
        settle(&mut w);
        none_out(&mut w);
        assert_eq!(read(&mut w, &copy), Some(sent));
        w.server.release(copy);
        let (free, all) = w.server.usage().frames();
        assert_eq!(free, all);
    }
}

#[test]
fn hold_without_frame_memory_leaves_the_frame_as_it_is() {
    // As many frame buffers as connections: holding them all leaves none.
    let tight = super::paged::paged(64, 8, 256).with_frames(6, 8192);
    let mut w = world_with(3, Faults::NONE, tight);
    let pair = paired(&mut w);
    let (f, sent) = one_in_pages(&mut w, pair);
    let mut taken = Vec::new();
    while let Some(ext) = w.server.extents().take() {
        taken.push(ext);
    }
    assert_eq!(w.server.hold(&mut w.io, f), Err(NetError::NoMemory));
    assert_eq!(read(&mut w, &f), Some(sent));
    for ext in taken {
        let _freed: bool = w.server.extents().release(ext).unwrap();
    }
    w.server.release(f);
    settle(&mut w);
    none_out(&mut w);
}

#[test]
fn a_frame_in_pages_is_not_sent_lent() {
    let mut w = world(5, Faults::NONE);
    let pair = paired(&mut w);
    let (f, _) = one_in_pages(&mut w, pair);
    assert_eq!(w.server.send_lent(pair.1, &f), Err(SendError::NotHeld));
    w.server.release(f);
    settle(&mut w);
    none_out(&mut w);
}

#[test]
fn a_frame_of_a_closed_connection_stays_until_its_holds_are_gone() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let (c, s) = paired(&mut w);
        let (f, sent) = one_in_pages(&mut w, (c, s));
        let run = f.run().unwrap();
        let write = w.server.runs().hold(run).unwrap();
        w.server.close(&mut w.io, s).unwrap();
        w.client.close(&mut w.io, c).unwrap();
        for _ in 0..1000 {
            if w.server.usage().connections() + w.client.usage().connections() == 0 {
                break;
            }
            w.turn();
            while w.client.next_event().is_some() || w.server.next_event().is_some() {}
        }
        assert_eq!(w.server.usage().connections(), 0);
        assert_eq!(read(&mut w, &f), Some(sent));
        w.server.release(f);
        settle(&mut w);
        assert!(out(&mut w) > 0, "the write still holds the run");
        assert_eq!(w.server.runs().release(write), Ok(true));
        settle(&mut w);
        none_out(&mut w);
    }
}

#[test]
fn a_set_given_back_whole_returns_every_piece() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let (f, _) = one_in_pages(&mut w, pair);
        let _write = w.server.runs().hold(f.run().unwrap()).unwrap();
        // And frames on their way, with pieces in runs and headers.
        for i in 0..3 {
            let _queued: bool = send(&mut w, pair.0, &prefixed(4092, i));
        }
        for _ in 0..3 {
            w.turn();
        }
        w.server.give_back_all(&mut w.io);
        assert!(w.server.pages(&f).is_none());
        assert_eq!(w.io.pieces_out(), (0, 0));
    }
}

#[test]
fn two_writes_hold_the_pages_until_both_are_done() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let (f, _) = one_in_pages(&mut w, pair);
        let run = f.run().unwrap();
        let first = w.server.runs().hold(run).unwrap();
        let second = w.server.runs().hold(run).unwrap();
        w.server.release(f);
        assert_eq!(w.server.runs().release(first), Ok(false));
        settle(&mut w);
        assert!(out(&mut w) > 0, "the second write still reads the pages");
        assert_eq!(w.server.runs().release(second), Ok(true));
        settle(&mut w);
        none_out(&mut w);
    }
}

#[test]
fn a_hold_given_back_after_the_set_gave_back_all_is_stale() {
    for seed in 0..10 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let (f, _) = one_in_pages(&mut w, pair);
        let write = w.server.runs().hold(f.run().unwrap()).unwrap();
        w.server.give_back_all(&mut w.io);
        assert_eq!(w.server.runs().release(write), Err(NetError::Stale));
        assert_eq!(w.io.pieces_out(), (0, 0));
    }
}
