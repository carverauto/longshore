// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Refusals at the call (C11), each of which submits nothing, takes no slot
//! and no hold, and reports nothing: a stale handle, ranges past their
//! extents and lengths of 0 (C9), more spans or steps than planned, bad
//! paths, stale extents, an extent with every hold taken, and no free file
//! slot.

use super::{bytes, limits, must, rw_create, sizes, tickets, Rig, EXTENT};
use crate::{FileError, Span, Step};
use longshore_common::io::{Io, Limits};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    stale_handles(make, root);
    past_the_plan_and_the_extent(make, root);
    bad_paths(make, root);
    extents_that_cannot_be_held(make, root);
}

/// Every operation of a closed file is `Stale`, a sequence with one too.
fn stale_handles<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}stale.dat"), rw_create());
    assert_eq!(rig.close(f), Ok(0));
    let e = rig.extent();
    let stale = FileError::Stale;
    rig.refused("read", stale, |s, io, x| s.read_at(io, x, f, (e, 0, 1), 0));
    rig.refused("write", stale, |s, io, x| {
        s.write_at(io, x, f, (e, 0, 1), 0)
    });
    let one = [Span::Extent {
        extent: e,
        at: 0,
        len: 1,
    }];
    rig.refused("spans", stale, |s, io, x| s.write_spans(io, x, f, &one, 0));
    rig.refused("sync", stale, |s, io, _| s.sync(io, f, true));
    rig.refused("reserve", stale, |s, io, _| s.reserve(io, f, (0, 1), true));
    rig.refused("punch", stale, |s, io, _| s.punch(io, f, (0, 4096)));
    rig.refused("close", stale, |s, io, _| s.close(io, f));
    let steps = [Step::Sync {
        file: f,
        data_only: true,
    }];
    let mut ts = tickets::<1>();
    rig.refused("a step", stale, |s, io, x| {
        s.sequence(io, x, &steps, &mut ts)
    });
    rig.release(e);
    rig.at_rest(0);
}

/// `TooLarge`: ranges past their extent, lengths of 0 for reserve and
/// punch, more spans than `spans_max`, more steps than `sequence_max`.
fn past_the_plan_and_the_extent<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}large.dat"), rw_create());
    let e = rig.extent();
    let big = FileError::TooLarge;
    let past = (e, EXTENT - 10, 11);
    rig.refused("write past", big, |s, io, x| s.write_at(io, x, f, past, 0));
    rig.refused("read past", big, |s, io, x| s.read_at(io, x, f, past, 0));
    let beyond = (e, EXTENT, 1);
    rig.refused("at past", big, |s, io, x| s.write_at(io, x, f, beyond, 0));
    rig.refused("reserve 0", big, |s, io, _| s.reserve(io, f, (0, 0), false));
    rig.refused("punch 0", big, |s, io, _| s.punch(io, f, (0, 0)));
    let span = Span::Extent {
        extent: e,
        at: 0,
        len: 8,
    };
    let five = [span; 5];
    rig.refused("5 spans", big, |s, io, x| s.write_spans(io, x, f, &five, 0));
    let off_end = [
        span,
        Span::Extent {
            extent: e,
            at: EXTENT - 4,
            len: 8,
        },
    ];
    rig.refused("a span past", big, |s, io, x| {
        s.write_spans(io, x, f, &off_end, 0)
    });
    let sync = Step::Sync {
        file: f,
        data_only: true,
    };
    let four = [sync; 4];
    let mut ts = tickets::<4>();
    rig.refused("4 steps", big, |s, io, x| s.sequence(io, x, &four, &mut ts));
    // At the bounds themselves, accepted.
    rig.fill(e, 0, &bytes(1, EXTENT as usize));
    let last = (e, EXTENT - 10, 10);
    assert_eq!(
        rig.run("write", |s, io, x| s.write_at(io, x, f, last, 0)),
        Ok(10)
    );
    let four_spans = [span; 4];
    let spanned = must(
        rig.call(|s, io, x| s.write_spans(io, x, f, &four_spans, 0)),
        "4 spans",
    );
    assert_eq!(rig.done(spanned), Ok(32));
    let three = [sync; 3];
    let mut ts3 = tickets::<3>();
    let n = rig.call(|s, io, x| s.sequence(io, x, &three, &mut ts3));
    assert_eq!(n, Ok(3));
    for each in ts3 {
        assert_eq!(rig.done(each), Ok(0));
    }
    rig.release(e);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// `BadPath`: a path one byte longer than `path_max`, or with a NUL in it,
/// for every operation that names one; one of `path_max` bytes is taken.
fn bad_paths<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let max = rig.config().path_max() as usize;
    let fits = format!("{root}{}", "x".repeat(max.saturating_sub(root.len())));
    assert_eq!(fits.len(), max);
    let long = format!("{fits}y");
    let nul = format!("{root}a\0b");
    let ok = format!("{root}fine.dat");
    let bad = FileError::BadPath;
    for p in [&long, &nul] {
        rig.refused("open", bad, |s, io, _| s.open(io, p, rw_create()));
        rig.refused("status", bad, |s, io, _| s.status(io, p));
        rig.refused("unlink", bad, |s, io, _| s.unlink(io, p));
        rig.refused("mkdir", bad, |s, io, _| s.mkdir(io, p));
        rig.refused("rename from", bad, |s, io, _| s.rename(io, p, &ok));
        rig.refused("rename to", bad, |s, io, _| s.rename(io, &ok, p));
        let steps = [Step::Status { path: p }];
        let mut ts = tickets::<1>();
        rig.refused("a step", bad, |s, io, x| s.sequence(io, x, &steps, &mut ts));
    }
    // Accepted: whatever the backend says of so long a name.
    let t = must(rig.call(|s, io, _| s.status(io, &fits)), "path_max bytes");
    let r = rig.done(t);
    assert!(r != Err(bad), "{r:?}");
    rig.at_rest(0);
}

/// `StaleExtent` for an extent let go of, as a range, a span or a step;
/// `TooManyHolds` for one held as often as can be counted; and `Full`
/// for an open when every file slot is taken.
fn extents_that_cannot_be_held<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}held.dat"), rw_create());
    let g = rig.open(&format!("{root}held2.dat"), rw_create());
    rig.refused("no file slot", FileError::Full, |s, io, _| {
        s.open(io, &format!("{root}held3.dat"), rw_create())
    });
    let gone = rig.extent();
    rig.release(gone);
    let stale = FileError::StaleExtent;
    rig.refused("write", stale, |s, io, x| {
        s.write_at(io, x, f, (gone, 0, 1), 0)
    });
    rig.refused("read", stale, |s, io, x| {
        s.read_at(io, x, f, (gone, 0, 1), 0)
    });
    let span = [Span::Extent {
        extent: gone,
        at: 0,
        len: 1,
    }];
    rig.refused("span", stale, |s, io, x| s.write_spans(io, x, f, &span, 0));
    let steps = [Step::Write {
        file: f,
        extent: gone,
        at: 0,
        len: 1,
        file_off: 0,
    }];
    let mut ts = tickets::<1>();
    rig.refused("a step", stale, |s, io, x| {
        s.sequence(io, x, &steps, &mut ts)
    });

    let full = rig.extent();
    for _ in 1..u16::MAX {
        must(rig.x().hold(full), "a hold");
    }
    let most = FileError::TooManyHolds;
    rig.refused("write", most, |s, io, x| {
        s.write_at(io, x, f, (full, 0, 1), 0)
    });
    for _ in 1..u16::MAX {
        rig.release(full);
    }
    assert_eq!(rig.x().holds(full), Some(1));
    rig.release(full);
    assert_eq!(rig.close(f), Ok(0));
    assert_eq!(rig.close(g), Ok(0));
    rig.at_rest(0);
}
