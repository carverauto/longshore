// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Every operation of C5 once, with its result: open with each flag,
//! close, read, write, sync, reserve, punch, status, rename, unlink,
//! mkdir, a directory's sync; what each event names; completions that are
//! not the set's.

use super::{bytes, fail, limits, must, rw_create, sizes, some, Rig, BLOCK};
use crate::{Event, FileError};
use longshore_common::io::{Io, Limits, Op, OpenFlags, Token};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    open_with_each_flag(make, root);
    write_sync_read_back(make, root);
    paths(make, root);
    space(make, root);
    events_name_what_they_are_of(make, root);
    completions_not_the_sets(make, root);
}

/// The directory `root` names, without its last `/`.
pub(super) fn dir_of(root: &str) -> &str {
    match root.trim_end_matches('/') {
        "" => "/",
        d => d,
    }
}

/// Read only, write only, create, exclusive, empty, durable writes, and a
/// directory.
fn open_with_each_flag<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let p = format!("{root}flags.dat");
    let ro = OpenFlags::NONE.with_read(true);
    let wo = OpenFlags::NONE.with_write(true);
    assert_eq!(rig.try_open(&p, ro), Err(FileError::NotFound));
    let f = rig.open(&p, rw_create());
    assert_eq!(rig.write(f, b"initial", 0), Ok(7));
    assert_eq!(rig.close(f), Ok(0));
    let excl = rw_create().with_exclusive(true);
    assert_eq!(rig.try_open(&p, excl), Err(FileError::Exists));

    let w = rig.open(&p, wo);
    let e = rig.extent();
    rig.refused(
        "a read of a write-only file",
        FileError::NotOpen,
        |s, io, x| s.read_at(io, x, w, (e, 0, 7), 0),
    );
    assert_eq!(rig.write(w, b"I", 0), Ok(1));
    assert_eq!(rig.close(w), Ok(0));

    let r = rig.open(&p, ro);
    rig.refused(
        "a write to a read-only file",
        FileError::NotOpen,
        |s, io, x| s.write_at(io, x, r, (e, 0, 7), 0),
    );
    rig.refused(
        "a reserve of a read-only file",
        FileError::NotOpen,
        |s, io, _| s.reserve(io, r, (0, 4096), false),
    );
    rig.refused(
        "a punch of a read-only file",
        FileError::NotOpen,
        |s, io, _| s.punch(io, r, (0, u64::from(BLOCK))),
    );
    rig.release(e);
    assert_eq!(rig.read(r, 0, 7), (Ok(7), b"Initial".to_vec()));
    assert_eq!(rig.sync(r, false), Ok(0));
    assert_eq!(rig.close(r), Ok(0));

    let emptied = rig.open(&p, OpenFlags::NONE.with_write(true).with_empty(true));
    assert_eq!(rig.close(emptied), Ok(0));
    assert_eq!(rig.status(&p), Ok(0));

    let durable = rig.open(&p, rw_create().with_dsync(true));
    let data = bytes(3, 3000);
    assert_eq!(rig.write(durable, &data, 100), Ok(3000));
    assert_eq!(rig.read(durable, 100, 3000), (Ok(3000), data));
    assert_eq!(rig.close(durable), Ok(0));

    let dir = rig.open(dir_of(root), ro.with_directory(true));
    assert_eq!(rig.sync(dir, false), Ok(0));
    assert_eq!(rig.close(dir), Ok(0));
    rig.at_rest(0);
}

/// Writes at offsets with a hole between them, both syncs, reads across
/// the end of the file and past it.
fn write_sync_read_back<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let p = format!("{root}rw.dat");
    let f = rig.open(&p, rw_create().with_empty(true));
    let (a, b) = (bytes(1, 5000), bytes(2, 3000));
    assert_eq!(rig.write(f, &a, 0), Ok(5000));
    assert_eq!(rig.write(f, &b, 9000), Ok(3000));
    let mark = rig.io().submitted();
    assert_eq!(rig.sync(f, true), Ok(0));
    assert_eq!(rig.sync(f, false), Ok(0));
    let syncs: Vec<_> = rig
        .io()
        .since(mark)
        .filter_map(|s| {
            if let Op::FileSync { datasync, .. } = s.op() {
                Some(datasync)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(syncs, [true, false], "data only, then data and metadata");
    let mut whole = a.clone();
    whole.resize(9000, 0);
    whole.extend_from_slice(&b);
    assert_eq!(rig.read(f, 0, 12_000), (Ok(12_000), whole.clone()));
    // Reads the backend completes short, before the end, are resumed:
    // halved three times, the read is sent four times at least.
    rig.io().cut_reads(3);
    let first = rig.io().submitted();
    assert_eq!(rig.read(f, 0, 12_000), (Ok(12_000), whole));
    let reads = rig
        .io()
        .since(first)
        .filter(|s| matches!(s.op(), Op::FileRead { .. }))
        .count();
    assert!(reads >= 4, "a read cut three times sent {reads} times");
    rig.io().cut_reads(1);
    assert_eq!(rig.read(f, 11_000, 4000), (Ok(1000), b[2000..].to_vec()));
    rig.io().cut_reads(0);
    // Fewer only at the end of the file; none past it.
    assert_eq!(rig.read(f, 11_000, 4000), (Ok(1000), b[2000..].to_vec()));
    assert_eq!(rig.read(f, 12_000, 100), (Ok(0), Vec::new()));
    assert_eq!(rig.status(&p), Ok(12_000));
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// mkdir, status, rename, unlink, and a directory synced after a create
/// and a rename in it.
fn paths<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let sub = format!("{root}sub");
    let (a, b) = (format!("{sub}/a.dat"), format!("{sub}/b.dat"));
    assert_eq!(rig.run("mkdir", |s, io, _| s.mkdir(io, &sub)), Ok(0));
    let again = rig.run("mkdir", |s, io, _| s.mkdir(io, &sub));
    assert_eq!(again, Err(FileError::Exists));
    let f = rig.open(&a, rw_create());
    assert_eq!(rig.write(f, b"kept!!", 0), Ok(6));
    assert_eq!(rig.sync(f, false), Ok(0));
    assert_eq!(rig.close(f), Ok(0));
    let dir = rig.open(&sub, OpenFlags::NONE.with_read(true).with_directory(true));
    assert_eq!(rig.sync(dir, false), Ok(0));
    assert_eq!(rig.status(&a), Ok(6));
    assert_eq!(rig.run("rename", |s, io, _| s.rename(io, &a, &b)), Ok(0));
    assert_eq!(rig.sync(dir, false), Ok(0));
    assert_eq!(rig.status(&a), Err(FileError::NotFound));
    assert_eq!(rig.status(&b), Ok(6));
    let missing = rig.run("rename", |s, io, _| s.rename(io, &a, &b));
    assert_eq!(missing, Err(FileError::NotFound));
    assert_eq!(rig.run("unlink", |s, io, _| s.unlink(io, &b)), Ok(0));
    let gone = rig.run("unlink", |s, io, _| s.unlink(io, &b));
    assert_eq!(gone, Err(FileError::NotFound));
    assert_eq!(rig.status(&b), Err(FileError::NotFound));
    let nowhere = format!("{root}no-such-dir/c.dat");
    assert_eq!(
        rig.try_open(&nowhere, rw_create()),
        Err(FileError::NotFound)
    );
    assert_eq!(rig.close(dir), Ok(0));
    rig.at_rest(0);
}

/// Reserve grows the file, or keeps its size; punch reads back as zeros
/// and keeps the size (a backend may not punch: macOS can refuse).
fn space<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let p = format!("{root}space.dat");
    let f = rig.open(&p, rw_create().with_empty(true));
    let grown = rig.run("reserve", |s, io, _| s.reserve(io, f, (0, 8192), false));
    assert_eq!(grown, Ok(0));
    assert_eq!(rig.status(&p), Ok(8192));
    let kept = rig.run("reserve", |s, io, _| s.reserve(io, f, (8192, 4096), true));
    assert_eq!(kept, Ok(0));
    assert_eq!(rig.status(&p), Ok(8192));
    assert_eq!(rig.write(f, &[0xAB; 8192], 0), Ok(8192));
    assert_eq!(rig.sync(f, true), Ok(0));
    match rig.run("punch", |s, io, _| s.punch(io, f, (4096, 4096))) {
        Ok(0) => {
            assert_eq!(rig.status(&p), Ok(8192));
            let (r, got) = rig.read(f, 0, 8192);
            assert_eq!(r, Ok(8192));
            assert!(got[..4096].iter().all(|b| *b == 0xAB), "kept");
            assert!(got[4096..].iter().all(|b| *b == 0), "punched");
        }
        Err(FileError::Unsupported) if cfg!(target_os = "macos") => {}
        other => fail(format_args!("punch: {other:?}")),
    }
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// An open's event names the new file; a file operation's, its file; a
/// path operation's, none.
fn events_name_what_they_are_of<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let p = format!("{root}named.dat");
    // A failed open names no file.
    let missing = format!("{root}named-missing.dat");
    let ro = OpenFlags::NONE.with_read(true);
    let failing = must(rig.call(|s, io, _| s.open(io, &missing, ro)), "open");
    let failed = rig.wait(failing).1;
    assert_eq!(failed, done(failing, None, Err(FileError::NotFound)));
    let opening = must(rig.call(|s, io, _| s.open(io, &p, rw_create())), "open");
    let Event::Done {
        ticket,
        file,
        result,
    } = rig.wait(opening).1;
    assert_eq!((ticket, result), (opening, Ok(0)));
    let f = some(file, "the opened file");
    let syncing = must(rig.call(|s, io, _| s.sync(io, f, true)), "sync");
    assert_eq!(rig.wait(syncing).1, done(syncing, Some(f), Ok(0)));
    let asking = must(rig.call(|s, io, _| s.status(io, &p)), "status");
    assert_eq!(rig.wait(asking).1, done(asking, None, Ok(0)));
    let closing = must(rig.call(|s, io, _| s.close(io, f)), "close");
    assert_eq!(rig.wait(closing).1, done(closing, Some(f), Ok(0)));
    rig.at_rest(0);
}

/// The event `Done` of these.
pub(super) const fn done(
    ticket: crate::Ticket,
    file: Option<crate::File>,
    result: Result<u64, FileError>,
) -> Event {
    Event::Done {
        ticket,
        file,
        result,
    }
}

/// A completion of an operation the set did not submit is left to its
/// owner: `on_completion` says it is not the set's.
fn completions_not_the_sets<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let c = rig.config();
    let mine = Token::new(c.token_base() + c.tokens() + 7);
    must(
        rig.io().submit(mine, Op::Timeout { nanos: 1000 }),
        "a timer",
    );
    let f = rig.open(&format!("{root}beside.dat"), rw_create());
    let mut seen = None;
    for _ in 0..10_000 {
        rig.turn();
        seen = rig.take_foreign();
        if seen.is_some() {
            break;
        }
    }
    assert_eq!(some(seen, "the timer's completion").token(), mine);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
