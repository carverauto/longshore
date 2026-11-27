// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A file slot's life (C3): a close is held back until the file's other
//! operations have reported, a closing file refuses everything with
//! `Closing`, the close frees the slot and raises its generation, as a
//! failed open does, and a handle kept past that is stale and never
//! reaches the file that has the slot now.

use super::{bytes, limits, must, rw_create, sizes, Rig};
use crate::{FileError, FileSizes};
use longshore_common::io::{Io, Limits, OpenFlags};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    close_is_held_back(make, root);
    generations(make, root);
}

/// A write and a sync in flight, then a close: nothing more is submitted
/// at the close, every call on the file is `Closing`, and the close is
/// reported after both and leaves the handle stale.
fn close_is_held_back<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}held-back.dat"), rw_create());
    let e = rig.extent();
    rig.fill(e, 0, &bytes(9, 3000));
    let w = must(
        rig.call(|s, io, x| s.write_at(io, x, f, (e, 0, 3000), 0)),
        "write",
    );
    let y = must(rig.call(|s, io, _| s.sync(io, f, false)), "sync");
    let before = rig.io().submitted();
    let c = must(rig.call(|s, io, _| s.close(io, f)), "close");
    assert_eq!(rig.io().submitted(), before, "the close is held back");
    let closing = FileError::Closing;
    rig.refused("read", closing, |s, io, x| {
        s.read_at(io, x, f, (e, 0, 1), 0)
    });
    rig.refused("write", closing, |s, io, x| {
        s.write_at(io, x, f, (e, 0, 1), 0)
    });
    rig.refused("sync", closing, |s, io, _| s.sync(io, f, true));
    rig.refused("reserve", closing, |s, io, _| {
        s.reserve(io, f, (0, 1), true)
    });
    rig.refused("punch", closing, |s, io, _| s.punch(io, f, (0, 4096)));
    rig.refused("close", closing, |s, io, _| s.close(io, f));
    let (at_w, done_w) = rig.wait(w);
    let (at_y, done_y) = rig.wait(y);
    let (at_c, done_c) = rig.wait(c);
    assert_eq!([done_w, done_y, done_c].map(super::ticket), [w, y, c]);
    assert!(at_c > at_w && at_c > at_y, "the close came last");
    let crate::Event::Done { result, .. } = done_c;
    assert_eq!(result, Ok(0));
    rig.refused("after the close", FileError::Stale, |s, io, _| {
        s.sync(io, f, true)
    });
    rig.release(e);
    rig.at_rest(0);
}

/// One file slot: each close and each failed open raises its generation;
/// the handle of the file before is stale and never reaches the one now.
fn generations<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let one = FileSizes::new(1, 2).with_shared(1).with_spans_max(4);
    let mut rig = Rig::new(make, limits(), one);
    let (p, q) = (format!("{root}gen-1.dat"), format!("{root}gen-2.dat"));
    let first = rig.open(&p, rw_create().with_empty(true));
    assert_eq!(rig.close(first), Ok(0));
    let second = rig.open(&q, rw_create().with_empty(true));
    assert_eq!(second.slot(), first.slot());
    assert_eq!(second.generation(), first.generation().wrapping_add(1));
    assert_eq!(rig.write(second, b"second", 0), Ok(6));
    let e = rig.extent();
    rig.fill(e, 0, b"FIRST!");
    rig.refused("the old handle", FileError::Stale, |s, io, x| {
        s.write_at(io, x, first, (e, 0, 6), 0)
    });
    rig.refused("its close", FileError::Stale, |s, io, _| s.close(io, first));
    rig.release(e);
    assert_eq!(rig.read(second, 0, 6), (Ok(6), b"second".to_vec()));
    assert_eq!(rig.close(second), Ok(0));
    let missing = format!("{root}gen-missing.dat");
    let ro = OpenFlags::NONE.with_read(true);
    assert_eq!(rig.try_open(&missing, ro), Err(FileError::NotFound));
    let third = rig.open(&p, ro);
    assert_eq!(third.generation(), second.generation().wrapping_add(2));
    assert_eq!(rig.close(third), Ok(0));
    rig.at_rest(0);
}
