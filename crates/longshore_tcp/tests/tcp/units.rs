// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The parts that do no I/O: handles, framing, backoff, deadlines.

use longshore_tcp::{Backoff, Deadlines, FrameError, FramingError, Generations, HeaderFraming};

#[test]
fn a_handle_is_stale_once_its_slot_is_retired() {
    let mut g = Generations::new(4);
    let old = g.conn(2);
    assert_eq!((old.slot(), g.slot_of(old)), (2, Some(2)));
    g.retire(2);
    let new = g.conn(2);
    assert_ne!(old, new);
    assert_eq!((g.slot_of(old), g.slot_of(new)), (None, Some(2)));
    // Other slots are untouched, and listeners follow the same rule.
    let l = g.listener(0);
    assert_eq!(g.slot_of_listener(l), Some(0));
    g.retire(0);
    assert_eq!(g.slot_of_listener(l), None);
}

#[test]
fn a_four_byte_prefix_gives_the_whole_frames_length() {
    let f = HeaderFraming::length_prefix(1 << 20).unwrap();
    assert_eq!(f.header_len(), 4);
    assert_eq!(f.frame_len(&[0, 0, 0, 0]), Ok(4));
    assert_eq!(f.frame_len(&[0, 0, 1, 0]), Ok(4 + 256));
    // The largest frame is accepted, one byte more is not.
    let most = (1u32 << 20) - 4;
    assert_eq!(f.frame_len(&most.to_be_bytes()), Ok(1 << 20));
    assert_eq!(
        f.frame_len(&(most + 1).to_be_bytes()),
        Err(FrameError::TooLarge)
    );
    assert_eq!(f.frame_len(&[0xff; 4]), Err(FrameError::TooLarge));
}

#[test]
fn a_size_inside_a_256_byte_header_counts_the_header() {
    // A 256-byte header whose little-endian u32 at offset 40 is the whole frame.
    let f = HeaderFraming::new(256, 40, 4, false, true, 1 << 20).unwrap();
    let mut header = [0u8; 256];
    let set = |h: &mut [u8; 256], size: u32| h[40..44].copy_from_slice(&size.to_le_bytes());
    set(&mut header, 256);
    assert_eq!(f.frame_len(&header), Ok(256));
    set(&mut header, 300_000);
    assert_eq!(f.frame_len(&header), Ok(300_000));
    set(&mut header, 1 << 20);
    assert_eq!(f.frame_len(&header), Ok(1 << 20));
    set(&mut header, (1 << 20) + 1);
    assert_eq!(f.frame_len(&header), Err(FrameError::TooLarge));
    // Shorter than its own header: not a frame.
    set(&mut header, 255);
    assert_eq!(f.frame_len(&header), Err(FrameError::TooSmall));
}

#[test]
fn every_width_and_byte_order_reads_its_field() {
    for (width, bytes, big, want) in [
        (1u8, &[0x7f][..], true, 0x7f_u64),
        (2, &[0x01, 0x02], true, 0x0102),
        (2, &[0x01, 0x02], false, 0x0201),
        (8, &[0, 0, 0, 0, 0, 0, 0x03, 0x04], true, 0x0304),
        (8, &[0x04, 0x03, 0, 0, 0, 0, 0, 0], false, 0x0304),
    ] {
        let max = if width == 1 { 255 } else { 65_535 };
        let f = HeaderFraming::new(u32::from(width), 0, width, big, true, max).unwrap();
        assert_eq!(
            f.frame_len(bytes),
            Ok(want as usize),
            "width {width}, big-endian {big}"
        );
    }
    // An eight-byte length too large for a frame is refused, not truncated.
    let f = HeaderFraming::new(8, 0, 8, true, true, 1 << 20).unwrap();
    assert_eq!(
        f.frame_len(&[1, 0, 0, 0, 0, 0, 0, 16]),
        Err(FrameError::TooLarge)
    );
}

#[test]
fn framing_descriptions_that_cannot_work_are_refused() {
    assert_eq!(
        HeaderFraming::new(8, 0, 3, true, true, 100).err(),
        Some(FramingError::Width)
    );
    assert_eq!(
        HeaderFraming::new(8, 6, 4, true, true, 100).err(),
        Some(FramingError::Field)
    );
    assert_eq!(
        HeaderFraming::new(0, 0, 4, true, true, 100).err(),
        Some(FramingError::Field)
    );
    // A maximum smaller than the header, or larger than the field can say.
    assert_eq!(
        HeaderFraming::new(8, 0, 4, true, true, 7).err(),
        Some(FramingError::MaxFrame)
    );
    assert_eq!(
        HeaderFraming::new(2, 0, 2, true, true, 65_536).err(),
        Some(FramingError::MaxFrame)
    );
    assert!(HeaderFraming::new(2, 0, 2, true, false, 65_537).is_ok());
}

#[test]
fn reconnect_delays_double_to_the_maximum_and_restart_after_a_reset() {
    const MS: u64 = 1_000_000;
    let mut b = Backoff::new(100 * MS, 5_000 * MS, 0, 42);
    let delays: Vec<u64> = (0..8).map(|_| b.next_delay_ns() / MS).collect();
    assert_eq!(delays, [100, 200, 400, 800, 1600, 3200, 5000, 5000]);
    assert_eq!(b.attempts(), 8);
    b.reset();
    assert_eq!(b.next_delay_ns(), 100 * MS);
    // Far past any shift that fits: still the maximum, never an overflow.
    let mut long = Backoff::new(100 * MS, 5_000 * MS, 0, 1);
    for _ in 0..200 {
        assert!(long.next_delay_ns() <= 5_000 * MS);
    }
}

#[test]
fn jitter_shortens_each_delay_by_at_most_its_share_and_follows_the_seed() {
    const MS: u64 = 1_000_000;
    let run = |seed| {
        let mut b = Backoff::new(100 * MS, 5_000 * MS, 250, seed);
        (0..10).map(|_| b.next_delay_ns()).collect::<Vec<u64>>()
    };
    let (a, again, other) = (run(7), run(7), run(8));
    assert_eq!(a, again, "the same seed gives the same delays");
    assert_ne!(a, other);
    let mut full = 100 * MS;
    for d in &a {
        assert!(
            *d <= full && *d >= full / 4 * 3,
            "{d} outside 75% to 100% of {full}"
        );
        full = (full * 2).min(5_000 * MS);
    }
}

#[test]
fn the_earliest_deadline_is_tracked_through_sets_and_clears() {
    let mut d = Deadlines::new(4);
    assert_eq!(d.earliest(), None);
    d.set(0, 500);
    d.set(1, 300);
    d.set(2, 900);
    assert_eq!(
        (d.earliest(), d.get(1), d.get(3)),
        (Some(300), Some(300), None)
    );
    // Clearing the earliest uncovers the next.
    d.clear(1);
    assert_eq!(d.earliest(), Some(500));
    // Moving the earliest later, and another earlier.
    d.set(0, 1000);
    assert_eq!(d.earliest(), Some(900));
    d.set(3, 10);
    assert_eq!(d.earliest(), Some(10));
    d.clear(3);
    d.clear(2);
    d.clear(0);
    assert_eq!(d.earliest(), None);
}

#[test]
fn due_deadlines_are_handed_out_once_each_in_slot_order() {
    let mut d = Deadlines::new(5);
    for (slot, at) in [(0, 50), (1, 200), (2, 50), (4, 100)] {
        d.set(slot, at);
    }
    assert_eq!(d.next_due(49), None);
    let due: Vec<u32> = std::iter::from_fn(|| d.next_due(100)).collect();
    assert_eq!(due, [0, 2, 4]);
    assert_eq!((d.earliest(), d.get(0)), (Some(200), None));
    assert_eq!(d.next_due(100), None, "nothing is due twice");
    assert_eq!(d.next_due(u64::MAX - 1), Some(1));
    assert_eq!(d.earliest(), None);
}
