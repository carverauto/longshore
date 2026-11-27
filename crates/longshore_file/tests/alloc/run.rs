// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The run the allocation guard watches: every operation of C5, refusals,
//! waiting, a sequence, a held-back close; and a power loss.

use longshore_common::io::{Io, OpenFlags};
use longshore_common::sim::SimIo;
use longshore_file::check::{rw_create, Rig, EXTENT};
use longshore_file::{FileError, Span, Step, Ticket};

/// The paths of a run, made before it.
pub struct Paths {
    a: String,
    b: String,
    c: String,
    sub: String,
    dir: String,
    missing: String,
    long: String,
}

impl Paths {
    pub fn under(root: &str, path_max: u32) -> Paths {
        let dir = match root.trim_end_matches('/') {
            "" => "/".to_string(),
            d => d.to_string(),
        };
        Paths {
            a: format!("{root}alloc-a.dat"),
            b: format!("{root}alloc-b.dat"),
            c: format!("{root}alloc-c.dat"),
            sub: format!("{root}alloc-sub"),
            dir,
            missing: format!("{root}alloc-missing.dat"),
            long: format!("{root}{}", "x".repeat(path_max as usize)),
        }
    }
}

/// Every operation once, with what it must give; `pages` when the backend
/// has a receive area to write a page of.
pub fn every_operation<IO: Io>(rig: &mut Rig<IO>, p: &Paths, pages: bool) {
    let f = rig.open(&p.a, rw_create().with_empty(true));
    let d = rig.open(&p.b, rw_create().with_empty(true).with_dsync(true));
    let (e1, e2, e3) = (rig.extent(), rig.extent(), rig.extent());
    for (i, b) in rig.io().buf(e1.buf()).iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7);
    }
    let wrote_all = rig.run("write", |s, io, x| s.write_at(io, x, f, (e1, 0, EXTENT), 0));
    assert_eq!(wrote_all, Ok(u64::from(EXTENT)));
    let wrote_durable = rig.run("durable", |s, io, x| {
        s.write_at(io, x, d, (e1, 100, 3000), 0)
    });
    assert_eq!(wrote_durable, Ok(3000));
    gathered(rig, f, (e1, e2), pages);
    let read_all = rig.run("read", |s, io, x| s.read_at(io, x, f, (e3, 0, EXTENT), 0));
    assert_eq!(read_all, Ok(u64::from(EXTENT)));
    let (wrote, read) = rig.io().buf_pair(e1.buf(), e3.buf());
    assert!(wrote == read, "read back");
    assert_eq!(rig.sync(f, true), Ok(0));
    assert_eq!(rig.sync(d, false), Ok(0));
    let reserved = rig.run("reserve", |s, io, _| {
        s.reserve(io, f, (40_000, 8192), false)
    });
    assert_eq!(reserved, Ok(0));
    let punched = rig.run("punch", |s, io, _| s.punch(io, f, (0, 4096)));
    assert!(
        punched == Ok(0) || punched == Err(FileError::Unsupported),
        "{punched:?}"
    );
    paths(rig, p);
    sequence(rig, f, (e1, e3));
    refusals(rig, f, e1, p);
    // Waiting for room, and a close held back behind a write.
    rig.io().set_room(Some(0));
    let w = rig.call(|s, io, x| s.write_at(io, x, d, (e1, 0, 512), 0));
    rig.io().set_room(None);
    let c = rig.call(|s, io, _| s.close(io, d));
    assert_eq!(rig.done(w.unwrap()), Ok(512));
    assert_eq!(rig.done(c.unwrap()), Ok(0));
    let root = rig.open(&p.dir, OpenFlags::NONE.with_read(true).with_directory(true));
    assert_eq!(rig.sync(root, false), Ok(0));
    assert_eq!(rig.close(root), Ok(0));
    assert_eq!(rig.close(f), Ok(0));
    rig.refused("stale", FileError::Stale, |s, io, _| s.sync(io, f, true));
    for e in [e1, e2, e3] {
        rig.release(e);
    }
    rig.at_rest(0);
}

/// A gathered write of two extents and, where there is one, a page.
fn gathered<IO: Io>(
    rig: &mut Rig<IO>,
    f: longshore_file::File,
    (e1, e2): (
        longshore_common::io::extents::Extent,
        longshore_common::io::extents::Extent,
    ),
    pages: bool,
) {
    // A page id only where the backend has a receive area; unused else.
    let page = if pages {
        rig.limits().page(0)
    } else {
        e1.buf()
    };
    let spans = [
        Span::Extent {
            extent: e1,
            at: 0,
            len: 1000,
        },
        Span::Extent {
            extent: e2,
            at: 4096,
            len: 2000,
        },
        Span::Page {
            page,
            off: 0,
            len: 500,
        },
    ];
    let n = if pages { 3 } else { 2 };
    let r = rig.run("spans", |s, io, x| {
        s.write_spans(io, x, f, &spans[..n], 20_000)
    });
    assert_eq!(r, Ok(if pages { 3500 } else { 3000 }));
}

/// status, mkdir, rename there and back, unlink of a missing path.
fn paths<IO: Io>(rig: &mut Rig<IO>, p: &Paths) {
    assert!(rig.status(&p.a).is_ok());
    let made = rig.run("mkdir", |s, io, _| s.mkdir(io, &p.sub));
    assert!(made == Ok(0) || made == Err(FileError::Exists), "{made:?}");
    assert_eq!(
        rig.run("rename", |s, io, _| s.rename(io, &p.a, &p.c)),
        Ok(0)
    );
    assert_eq!(
        rig.run("rename", |s, io, _| s.rename(io, &p.c, &p.a)),
        Ok(0)
    );
    let gone = rig.run("unlink", |s, io, _| s.unlink(io, &p.missing));
    assert_eq!(gone, Err(FileError::NotFound));
    assert_eq!(rig.status(&p.missing), Err(FileError::NotFound));
}

/// A write, a sync and a read as one sequence.
fn sequence<IO: Io>(
    rig: &mut Rig<IO>,
    f: longshore_file::File,
    (from, into): (
        longshore_common::io::extents::Extent,
        longshore_common::io::extents::Extent,
    ),
) {
    let steps = [
        Step::Write {
            file: f,
            extent: from,
            at: 0,
            len: 4096,
            file_off: 8192,
        },
        Step::Sync {
            file: f,
            data_only: true,
        },
        Step::Read {
            file: f,
            extent: into,
            at: 0,
            len: 4096,
            file_off: 8192,
        },
    ];
    let mut ts = [Ticket::NONE; 3];
    let n = rig.call(|s, io, x| s.sequence(io, x, &steps, &mut ts));
    assert_eq!(n, Ok(3));
    for (t, want) in ts.into_iter().zip([4096, 0, 4096]) {
        assert_eq!(rig.done(t), Ok(want));
    }
}

/// A range past its extent, a path too long, a sequence too long.
fn refusals<IO: Io>(
    rig: &mut Rig<IO>,
    f: longshore_file::File,
    e: longshore_common::io::extents::Extent,
    p: &Paths,
) {
    rig.refused("past", FileError::TooLarge, |s, io, x| {
        s.write_at(io, x, f, (e, EXTENT, 1), 0)
    });
    rig.refused("long", FileError::BadPath, |s, io, _| s.status(io, &p.long));
    let sync = Step::Sync {
        file: f,
        data_only: true,
    };
    let four = [sync; 4];
    let mut ts = [Ticket::NONE; 4];
    rig.refused("steps", FileError::TooLarge, |s, io, x| {
        s.sequence(io, x, &four, &mut ts)
    });
}

/// The power is lost with a write in flight: the set lets go of it all.
pub fn lose_power(rig: &mut Rig<SimIo>, p: &Paths) {
    let f = rig.open(&p.a, rw_create());
    let e = rig.extent();
    let w = rig.call(|s, io, x| s.write_at(io, x, f, (e, 0, 4096), 0));
    assert!(w.is_ok());
    rig.io().inner_mut().power_loss();
    rig.io().lost();
    rig.call(|s, _, x| s.power_lost(x));
    assert_eq!(rig.x().holds(e), Some(1));
    rig.release(e);
    rig.at_rest(0);
    rig.refused("lost", FileError::Stale, |s, io, _| s.sync(io, f, true));
}
