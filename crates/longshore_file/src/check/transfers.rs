// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Whole transfers (C6) and memory (C8): whole extents and parts of them,
//! several operations of one file in flight at once, gathered writes of
//! several extents, and each extent held by the set from acceptance to
//! report.

use super::{bytes, fail, limits, must, rw_create, sizes, Rig, EXTENT};
use crate::{Span, Ticket};
use longshore_common::io::{Io, Limits};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    whole_extents_and_parts(make, root);
    several_in_flight_on_one_file(make, root);
    gathered_writes(make, root);
}

/// A whole extent written at an odd offset and read back whole; a part
/// from inside an extent read into the middle of another.
fn whole_extents_and_parts<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}whole.dat"), rw_create().with_empty(true));
    let all = bytes(5, EXTENT as usize);
    assert_eq!(rig.write(f, &all, 1234), Ok(u64::from(EXTENT)));
    assert_eq!(
        rig.read(f, 1234, EXTENT),
        (Ok(u64::from(EXTENT)), all.clone())
    );

    // From the middle of an extent, held by the set alone while in flight.
    let e = rig.extent();
    rig.fill(e, 100, &all[..5000]);
    let t = must(
        rig.call(|s, io, x| s.write_at(io, x, f, (e, 100, 5000), 40_000)),
        "write_at",
    );
    rig.release(e);
    if rig.x().holds(e) != Some(1) {
        fail(format_args!("a write in flight from {e:?} is not held"));
    }
    assert_eq!(rig.done(t), Ok(5000));
    assert!(!rig.x().is_live(e), "the set let go of its extent");

    let into = rig.extent();
    let reading = must(
        rig.call(|s, io, x| s.read_at(io, x, f, (into, 300, 5000), 40_000)),
        "read_at",
    );
    if rig.x().holds(into) != Some(2) {
        fail(format_args!("a read in flight into {into:?} is not held"));
    }
    assert_eq!(rig.done(reading), Ok(5000));
    assert_eq!(rig.bytes(into, 300, 5000), all[..5000]);
    assert_eq!(
        rig.x().holds(into),
        Some(1),
        "only the caller's hold is left"
    );
    rig.release(into);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// Three writes of one file in flight at once, to ranges of their own, and
/// three reads of them, all whole.
fn several_in_flight_on_one_file<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}several.dat"), rw_create().with_empty(true));
    let mut tickets = [None::<Ticket>; 3];
    for (i, t) in tickets.iter_mut().enumerate() {
        let e = rig.extent();
        rig.fill(e, 0, &bytes(i as u8, 3000));
        let off = i as u64 * 3000;
        *t = Some(must(
            rig.call(|s, io, x| s.write_at(io, x, f, (e, 0, 3000), off)),
            "write_at",
        ));
        rig.release(e);
    }
    assert_eq!(rig.usage().ops(), 3, "three operations in flight");
    for t in tickets.iter().flatten() {
        assert_eq!(rig.done(*t), Ok(3000));
    }
    for i in 0..3u8 {
        let off = u64::from(i) * 3000;
        assert_eq!(rig.read(f, off, 3000), (Ok(3000), bytes(i, 3000)));
    }
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// Spans of three extents, from where each says, land in order at one
/// offset; each extent is held until the write's event and only then let
/// go of.
fn gathered_writes<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}spans.dat"), rw_create().with_empty(true));
    let parts: [(u32, u32); 3] = [(0, 1000), (500, 3000), (4096, 100)];
    let mut spans = [None::<Span>; 3];
    let mut want = Vec::new();
    for (i, (at, len)) in parts.iter().enumerate() {
        let e = rig.extent();
        let data = bytes(10 + i as u8, *len as usize);
        rig.fill(e, *at, &data);
        want.extend_from_slice(&data);
        spans[i] = Some(Span::Extent {
            extent: e,
            at: *at,
            len: *len,
        });
    }
    let list: Vec<Span> = spans.iter().flatten().copied().collect();
    let t = must(
        rig.call(|s, io, x| s.write_spans(io, x, f, &list, 777)),
        "write_spans",
    );
    for span in &list {
        if let Span::Extent { extent, .. } = *span {
            assert_eq!(rig.x().holds(extent), Some(2), "held while in flight");
            rig.release(extent);
        }
    }
    assert_eq!(rig.done(t), Ok(4100));
    for span in &list {
        if let Span::Extent { extent, .. } = *span {
            assert!(!rig.x().is_live(extent), "let go of after the event");
        }
    }
    assert_eq!(rig.read(f, 777, 4100), (Ok(4100), want));
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
