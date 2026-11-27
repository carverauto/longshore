// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! On the simulator: power loss and `power_lost` (C12), a close that fails
//! and frees its slot all the same (C3), and pages of the receive area as
//! spans of a gathered write (C5).

use super::sim::{place, rig, sim};
use longshore_common::io::{Io, OpenFlags};
use longshore_common::sim::PlacedFault;
use longshore_file::check::{self, bytes, must, rw_create, Rig};
use longshore_file::{FileError, Span};

const EIO: i32 = 5;

/// What was written, synced, and named by a synced directory is there
/// after the power is lost; what was in flight ends without an event, the
/// set holds nothing, and every handle is stale.
#[test]
fn power_loss_keeps_what_was_synced_and_ends_what_was_in_flight() {
    let mut rig = rig();
    let f = rig.open("/kept.dat", rw_create());
    let root = rig.open("/", OpenFlags::NONE.with_read(true).with_directory(true));
    assert_eq!(rig.sync(root, false), Ok(0));
    let kept = bytes(4, 6000);
    assert_eq!(rig.write(f, &kept, 0), Ok(6000));
    assert_eq!(rig.sync(f, true), Ok(0));
    let e = rig.extent();
    rig.fill(e, 0, &bytes(5, 3000));
    let lost = must(
        rig.call(|s, io, x| s.write_at(io, x, f, (e, 0, 3000), 9000)),
        "a write in flight",
    );
    assert_eq!(rig.x().holds(e), Some(2));
    rig.io().inner_mut().power_loss();
    rig.io().lost();
    rig.call(|s, _, x| s.power_lost(x));
    assert_eq!(rig.x().holds(e), Some(1), "the set let go of its hold");
    for _ in 0..20 {
        rig.turn();
    }
    assert!(
        rig.take_events().is_empty(),
        "{lost:?} ended without an event"
    );
    rig.at_rest(0);
    rig.refused("an old handle", FileError::Stale, |s, io, _| {
        s.sync(io, f, true)
    });
    rig.release(e);
    let again = rig.open("/kept.dat", OpenFlags::NONE.with_read(true));
    assert!(again.slot() != f.slot() || again.generation() != f.generation());
    assert_eq!(rig.read(again, 0, 6000), (Ok(6000), kept));
    assert_eq!(rig.close(again), Ok(0));
    rig.at_rest(0);
}

/// A close the backend fails (after it took effect) is reported with its
/// error, and the slot is free all the same.
#[test]
fn a_failed_close_frees_its_slot() {
    let mut rig = rig();
    let f = rig.open("/closing.dat", rw_create());
    place(&mut rig, 1, PlacedFault::FailAfter);
    assert_eq!(rig.close(f), Err(FileError::Io { errno: EIO }));
    rig.at_rest(0);
    rig.refused("its handle", FileError::Stale, |s, io, _| {
        s.sync(io, f, true)
    });
    let g = rig.open("/closing.dat", rw_create());
    let h = rig.open("/closing-2.dat", rw_create());
    assert_eq!(rig.close(g), Ok(0));
    assert_eq!(rig.close(h), Ok(0));
    rig.at_rest(0);
}

/// A gathered write of an extent and a page of the receive area, which
/// the caller holds: both land, in order, and the set holds the extent
/// only.
#[test]
fn a_page_of_the_receive_area_is_a_span() {
    let base = check::limits().with_area_pages(2);
    let mut rig = Rig::new(&mut sim, base, check::sizes());
    let f = rig.open("/paged.dat", rw_create().with_empty(true));
    let e = rig.extent();
    let (head, tail) = (bytes(6, 700), bytes(7, 1500));
    rig.fill(e, 0, &head);
    let page = rig.limits().page(1);
    rig.io().inner_mut().buf(page)[100..1600].copy_from_slice(&tail);
    let spans = [
        Span::Extent {
            extent: e,
            at: 0,
            len: 700,
        },
        Span::Page {
            page,
            off: 100,
            len: 1500,
        },
    ];
    let t = must(
        rig.call(|s, io, x| s.write_spans(io, x, f, &spans, 50)),
        "spans",
    );
    assert_eq!(rig.x().holds(e), Some(2));
    assert_eq!(rig.done(t), Ok(2200));
    assert_eq!(rig.x().holds(e), Some(1));
    let mut want = head;
    want.extend_from_slice(&tail);
    assert_eq!(rig.read(f, 50, 2200), (Ok(2200), want));
    rig.release(e);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
