// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving in pages when something runs out (C11): the page budget,
//! runs and tail blocks, piece records; the completion ring; pages for
//! the kernel's copy. Frames are moved instead of kept, receives are
//! armed again, and every frame arrives all the same.

use super::paged::{exchange, mixed, none_out, paged, paired, settle, world_with, zc};
use super::*;

/// The set's counters of several runs added up: frames kept and moved,
/// receives short of a page, receives armed again.
#[derive(Clone, Copy, Debug, Default)]
struct Sum(u64, u64, u64, u64);

impl Sum {
    const fn frames_kept(self) -> u64 {
        self.0
    }

    const fn frames_moved(self) -> u64 {
        self.1
    }

    const fn area_short(self) -> u64 {
        self.2
    }

    const fn rearmed(self) -> u64 {
        self.3
    }
}

/// Every frame of `mixed` arrives under `server` sizes and `faults`, and
/// every piece is back at the end: what the set counted.
fn all_arrive(seeds: core::ops::Range<u64>, faults: Faults, server: Sizes) -> Sum {
    all_arrive_in(seeds, faults, server, None)
}

/// [`all_arrive`], on an area of `area` pages when given: fewer than the
/// plan asks for, so that the copy path's quarter of it runs dry while
/// returns wait to be read.
fn all_arrive_in(
    seeds: core::ops::Range<u64>,
    faults: Faults,
    server: Sizes,
    area: Option<u32>,
) -> Sum {
    let mut sum = Sum::default();
    for seed in seeds {
        let mut w = match area {
            Some(pages) => small_area(seed, faults, server, pages),
            None => world_with(seed, faults, server),
        };
        let pair = paired(&mut w);
        let _none: Vec<Frame> = exchange(&mut w, pair, &mixed(seed as u8), |_| false);
        settle(&mut w);
        none_out(&mut w);
        let p = w.server.usage().pages();
        sum.0 += p.frames_kept();
        sum.1 += p.frames_moved();
        sum.2 += p.area_short();
        sum.3 += p.rearmed();
    }
    sum
}

#[test]
fn a_budget_of_one_page_keeps_only_frames_of_one_page() {
    // Frames of two pages never fit; frames of one page do.
    let p = all_arrive(0..30, Faults::NONE, paged(1, 8, 256));
    assert!(p.frames_kept() > 0 && p.frames_moved() > 0, "{p:?}");
    let q = all_arrive(0..30, Faults::NONE, paged(64, 8, 256));
    assert!(q.frames_kept() > p.frames_kept(), "{p:?} against {q:?}");
}

#[test]
fn two_piece_records_break_frames_of_more_pages() {
    // A page is a header and one more piece at best: two records.
    let p = all_arrive(0..30, Faults::NONE, paged(64, 8, 2));
    let q = all_arrive(0..30, Faults::NONE, paged(64, 8, 256));
    assert!(p.frames_kept() < q.frames_kept(), "{p:?} against {q:?}");
}

#[test]
fn one_run_is_one_frame_in_pages_at_a_time() {
    for seed in 0..20 {
        let mut w = world_with(seed, Faults::NONE, paged(64, 1, 256).with_frames(16, 8192));
        let pair = paired(&mut w);
        let held = exchange(&mut w, pair, &mixed(seed as u8), |_| true);
        let in_pages = held.iter().filter(|f| f.run().is_some()).count();
        assert!(in_pages <= 1, "{in_pages} frames in pages with one run");
        for f in held {
            w.server.release(f);
        }
        settle(&mut w);
        none_out(&mut w);
    }
}

#[test]
fn a_full_completion_ring_is_armed_again_and_a_silent_receive_woken() {
    let faults = Faults::NONE.with_ring_full(200_000);
    let p = all_arrive(0..40, faults, paged(64, 8, 256));
    assert!(p.rearmed() > 0 && p.frames_kept() > 0, "{p:?}");
}

/// A driver that ticks twice before it polls the sets again: that a tick
/// found the completion ring full is not lost in the second, and the
/// receive it left silent is woken all the same.
#[test]
fn a_full_ring_seen_two_ticks_before_a_poll_still_wakes_the_receive() {
    let mut rearmed = 0;
    for seed in 0..40 {
        let faults = Faults::NONE.with_ring_full(200_000);
        let mut w = world_with(seed, faults, paged(64, 8, 256));
        w.ticks = 2;
        let pair = paired(&mut w);
        let _none: Vec<Frame> = exchange(&mut w, pair, &mixed(seed as u8), |_| false);
        settle(&mut w);
        none_out(&mut w);
        rearmed += w.server.usage().pages().rearmed();
    }
    assert!(rearmed > 0, "no receive was woken");
}

/// A world whose backend has an area of `pages` pages.
fn small_area(seed: u64, faults: Faults, server: Sizes, pages: u32) -> World {
    let w = world_with(seed, faults, server);
    let limits = w.limits.with_area_pages(pages);
    World {
        io: SimIo::new(limits, seed, faults),
        limits,
        ..w
    }
}

#[test]
fn the_copy_path_without_pages_is_asked_again() {
    let faults = Faults::NONE.with_copied(300_000);
    let p = all_arrive_in(0..40, faults, paged(64, 8, 256), Some(12));
    assert!(p.area_short() > 0, "{p:?}");
}

#[test]
fn frames_arrive_under_every_fault_of_the_network_and_the_area() {
    let faults = Faults::NONE
        .with_reorder(300_000)
        .with_short_send(200_000)
        .with_partial_recv(300_000)
        .with_delay(100_000)
        .with_copied(50_000)
        .with_ring_full(50_000);
    let p = all_arrive(0..60, faults, paged(64, 8, 256));
    assert!(p.frames_kept() > 0 && p.frames_moved() > 0, "{p:?}");
}

#[test]
fn a_frame_shorter_than_the_least_is_moved() {
    let p = all_arrive(
        0..20,
        Faults::NONE,
        paged(64, 8, 256).with_zc_min_frame(8192),
    );
    // Only frames of two pages, which `mixed` has two of, are kept.
    assert!(
        p.frames_kept() <= 2 * 20 && p.frames_moved() >= 8 * 20,
        "{p:?}"
    );
}

/// Send frames of two pages with every piece through the copy path, on
/// an area whose copy path has two pages: the list runs out. How the server's
/// connection ended, if it did, and what the set counted.
fn hold_until_dry(seed: u64) -> (Option<Why>, longshore_tcp::Paged) {
    let faults = Faults::NONE.with_copied(1_000_000);
    let mut w = small_area(seed, faults, paged(64, 32, 256), 8);
    // Ten steps of the simulator without a page end the connection.
    let _: Listener = w.listen(ListenConfig::new(zc().with_no_page_timeout_ns(MS)));
    let (c, s) = w.connect(prefix());
    let frame = prefixed(8188, seed as u8);
    let (mut sent, mut held, mut closed) = (0, Vec::new(), None);
    for _ in 0..100_000 {
        if closed.is_some() || held.len() == 40 {
            break;
        }
        if sent < 40 && send(&mut w, c, &frame) {
            sent += 1;
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while let Some(e) = w.server.next_event() {
            if let Event::Closed { why, .. } = e {
                closed = Some(why);
                continue;
            }
            while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
                held.push(f);
            }
        }
    }
    let p = w.server.usage().pages();
    for f in held {
        w.server.release(f);
    }
    settle(&mut w);
    none_out(&mut w);
    (closed, p)
}

#[test]
fn a_copy_path_dry_for_longer_than_the_timeout_ends_the_connection() {
    let mut ended = 0;
    for seed in 0..10 {
        let (closed, p) = hold_until_dry(seed);
        if closed.is_some() {
            assert_eq!(closed, Some(Why::Reset));
            assert!(p.area_short() > 0, "{p:?}");
            ended += 1;
        }
    }
    assert!(ended > 0, "the copy path never ran dry for long");
}

/// The copy path running dry for spells shorter than the timeout of one
/// second, every frame released as it arrives: more ends without a page
/// than the sixteen tries the bound once was, and the connection lives,
/// since the bound is in time and a piece starts it again.
#[test]
fn a_copy_path_dry_for_less_than_the_timeout_keeps_the_connection() {
    let mut beyond = 0;
    for seed in 0..10 {
        let faults = Faults::NONE.with_copied(1_000_000);
        let mut w = small_area(seed, faults, paged(64, 32, 256), 8);
        let (c, s) = paired(&mut w);
        let frame = prefixed(8188, seed as u8);
        let (mut sent, mut got) = (0, 0);
        for _ in 0..200_000 {
            if got == 60 {
                break;
            }
            if sent < 60 && send(&mut w, c, &frame) {
                sent += 1;
            }
            w.turn();
            while w.client.next_event().is_some() {}
            while let Some(e) = w.server.next_event() {
                assert!(!matches!(e, Event::Closed { .. }), "seed {seed}: {e:?}");
                while let Some(f) = w.server.next_frame(&mut w.io, s).unwrap() {
                    got += 1;
                    w.server.release(f);
                }
            }
        }
        assert_eq!(got, 60, "seed {seed}");
        settle(&mut w);
        none_out(&mut w);
        beyond += u32::from(w.server.usage().pages().area_short() > 16);
    }
    assert!(beyond > 0, "never more than sixteen ends without a page");
}
