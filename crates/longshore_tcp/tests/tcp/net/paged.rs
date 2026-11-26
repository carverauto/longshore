// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving in pages (`Rx::ZeroCopy`) on the simulator's area: frames
//! kept in their pages and frames moved arrive whole and in order, every
//! piece goes back once, and a frame in pages is held, released and
//! read as the contract says. The simulator stops a run in which a piece
//! is returned that is not out, so every test here is also a test of
//! that; and a returned piece's bytes are overwritten, so a frame read
//! after its pages went back shows it.

use super::*;
use longshore_tcp::{Paged, Span};

/// Server sizes with pages for the mode: frame buffers for every
/// connection and two more.
pub(super) fn paged(pages: u32, runs: u32, records: u32) -> Sizes {
    // Segments as large as the client's: the class has one size.
    sizes(2, 4, 2)
        .with_segments(8, 8192)
        .with_zc(pages, runs, records)
}

pub(super) fn world_with(seed: u64, faults: Faults, server: Sizes) -> World {
    // Segments a frame of 8 KiB fits in.
    let client = sizes(4, 0, 0).with_segments(8, 8192);
    World::with(seed, faults, client, server)
}

pub(super) fn world(seed: u64, faults: Faults) -> World {
    world_with(seed, faults, paged(64, 8, 256))
}

/// Receiving in pages, with frames of a four-byte length prefix.
pub(super) fn zc() -> ConnOpts {
    prefix().with_rx(Rx::ZeroCopy)
}

/// A listener that reads in pages, and a connection to it from a client
/// that sends as usual.
pub(super) fn paired(w: &mut World) -> (Conn, Conn) {
    let _: Listener = w.listen(ListenConfig::new(zc()));
    w.connect(prefix())
}

/// The bytes of `frame`, wherever they lie; `None` once released.
pub(super) fn read(w: &mut World, frame: &Frame) -> Option<Vec<u8>> {
    if let Some(spans) = w.server.pages(frame) {
        let mut all = Vec::new();
        for span in spans {
            let (at, n) = (span.off() as usize, span.len() as usize);
            all.extend_from_slice(&w.io.buf(span.buf())[at..at + n]);
        }
        return Some(all);
    }
    w.server.bytes(&mut w.io, frame).map(<[u8]>::to_vec)
}

/// Send `frames` and pull them as they come, checking each; frames that
/// `keep` says so of are kept unreleased and returned.
pub(super) fn exchange(
    w: &mut World,
    (c, s): (Conn, Conn),
    frames: &[Vec<u8>],
    keep: impl Fn(usize) -> bool,
) -> Vec<Frame> {
    let (mut next, mut got, mut kept) = (0, 0, Vec::new());
    for _ in 0..200_000 {
        if got == frames.len() {
            return kept;
        }
        while next < frames.len() && send(w, c, &frames[next]) {
            next += 1;
        }
        w.turn();
        while let Some(e) = w.client.next_event() {
            assert!(matches!(e, Event::Writable(_)), "{e:?}");
        }
        while let Some(e) = w.server.next_event() {
            assert_eq!(e, Event::Readable(s));
            while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
                assert_eq!(
                    read(w, &f).as_deref(),
                    Some(&frames[got][..]),
                    "frame {got}"
                );
                assert!(f.is_held());
                if keep(got) {
                    kept.push(f);
                } else {
                    w.server.release(f);
                }
                got += 1;
            }
        }
    }
    panic!("{got} of {} frames arrived", frames.len());
}

/// Frames of whole pages, which the simulator often lays from the start
/// of a page; frames with a partial last block; frames too short to keep.
pub(super) fn mixed(seed: u8) -> Vec<Vec<u8>> {
    [4092, 8188, 5000, 4092, 100, 6000, 4092, 4092, 3, 8188]
        .iter()
        .enumerate()
        .map(|(i, n)| prefixed(*n, seed ^ i as u8))
        .collect()
}

/// Turn a few times, so that what was given up goes back.
pub(super) fn settle(w: &mut World) {
    for _ in 0..4 {
        w.turn();
    }
}

/// Neither the set nor the simulator has a piece out.
pub(super) fn none_out(w: &mut World) {
    let p: Paged = w.server.usage().pages();
    assert_eq!((p.runs(), p.pieces(), p.pages().0), (0, 0, 0), "{p:?}");
    assert_eq!(w.io.pieces_out(), (0, 0));
}

#[test]
fn frames_kept_and_moved_arrive_whole_and_every_piece_goes_back() {
    let (mut kept, mut moved) = (0, 0);
    for seed in 0..40 {
        let mut w = world(seed, Faults::NONE);
        let pair = paired(&mut w);
        let frames = mixed(seed as u8);
        let _none: Vec<Frame> = exchange(&mut w, pair, &frames, |_| false);
        settle(&mut w);
        none_out(&mut w);
        let p = w.server.usage().pages();
        assert_eq!(p.frames_kept() + p.frames_moved(), frames.len() as u64);
        // A kept frame copies its header and last block, a moved one all.
        assert!(
            p.moved_bytes() < frames.iter().map(|f| f.len() as u64).sum::<u64>()
                || p.frames_kept() == 0
        );
        (kept, moved) = (kept + p.frames_kept(), moved + p.frames_moved());
    }
    assert!(kept > 40 && moved > 40, "kept {kept}, moved {moved}");
}

#[test]
fn a_frame_in_pages_lies_in_the_area_and_its_tail_block_is_padded() {
    let mut seen = 0;
    for seed in 0..40 {
        // Every frame is held: a frame buffer for each moved one.
        let mut w = world_with(seed, Faults::NONE, paged(64, 8, 256).with_frames(16, 8192));
        let pair = paired(&mut w);
        let frames = mixed(seed as u8);
        let held = exchange(&mut w, pair, &frames, |_| true);
        for f in &held {
            let Some(spans) = w.server.pages(f) else {
                assert!(f.run().is_none() && f.span().is_some());
                continue;
            };
            seen += 1;
            assert!(f.span().is_none() && f.extent().is_none());
            assert!(w.server.bytes(&mut w.io, f).is_none());
            let spans: Vec<Span> = spans.collect();
            let (last, pages) = spans.split_last().unwrap();
            let limits = w.limits;
            for span in pages {
                assert!(limits.is_page(span.buf()), "{span:?}");
                assert_eq!((span.off(), span.len()), (0, Limits::PAGE));
            }
            if f.len() % Limits::PAGE == 0 {
                assert!(limits.is_page(last.buf()));
            } else {
                // Its own block, zero beyond the frame.
                assert!(!limits.is_page(last.buf()));
                let end = (last.off() + Limits::PAGE) as usize;
                let pad = &w.io.buf(last.buf())[(last.off() + last.len()) as usize..end];
                assert!(pad.iter().all(|b| *b == 0));
            }
        }
        for f in held {
            w.server.release(f);
        }
        settle(&mut w);
        none_out(&mut w);
    }
    assert!(seen > 40, "{seen} frames in pages");
}
