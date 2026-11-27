// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ordered sequences (C7): steps go to the backend as one chain in their
//! order, each starts after the one before is whole, a failed step ends
//! the rest as `Cancelled` and they are not performed, a short read at the
//! end of a file is not a failure, and the steps may be of several files
//! and of paths.

use super::{bytes, fail, limits, must, rw_create, sizes, Kind, Rig};
use crate::{Event, File, FileError, Step, Ticket};
use longshore_common::io::extents::Extent;
use longshore_common::io::{Io, Limits};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    write_sync_read_in_order(make, root);
    a_failure_ends_the_rest(make, root);
    a_short_read_is_not_a_failure(make, root);
    steps_of_several_files_and_a_path(make, root);
    steps_behind_a_path_step_wait_for_it(make, root);
}

/// Run `steps` as a sequence; the result of each, in step order, and
/// whether their events came in that order.
pub(super) fn results<IO: Io>(
    rig: &mut Rig<IO>,
    steps: &[Step<'_>],
) -> (Vec<Result<u64, FileError>>, bool) {
    let mut ts = [Ticket::NONE; 8];
    let n = must(
        rig.call(|s, io, x| s.sequence(io, x, steps, &mut ts)),
        "sequence",
    );
    assert_eq!(n, steps.len(), "a ticket for every step");
    let mut got = Vec::new();
    let mut order = Vec::new();
    for t in &ts[..n] {
        let (seq, Event::Done { result, .. }) = rig.wait(*t);
        got.push(result);
        order.push(seq);
    }
    (got, order.windows(2).all(|w| w[0] < w[1]))
}

fn write_step(file: File, extent: Extent, len: u32, file_off: u64) -> Step<'static> {
    Step::Write {
        file,
        extent,
        at: 0,
        len,
        file_off,
    }
}

fn read_step(file: File, extent: Extent, len: u32, file_off: u64) -> Step<'static> {
    Step::Read {
        file,
        extent,
        at: 0,
        len,
        file_off,
    }
}

/// A write, a sync and a read of what was written: one chain of three, in
/// step order, and the read sees the write.
fn write_sync_read_in_order<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}seq.dat"), rw_create().with_empty(true));
    let (from, into) = (rig.extent(), rig.extent());
    let data = bytes(7, 4096);
    rig.fill(from, 0, &data);
    let mark = rig.io().submitted();
    let steps = [
        write_step(f, from, 4096, 0),
        Step::Sync {
            file: f,
            data_only: true,
        },
        read_step(f, into, 4096, 0),
    ];
    let (got, ordered) = results(&mut rig, &steps);
    assert_eq!(got, [Ok(4096), Ok(0), Ok(4096)]);
    assert!(ordered, "events in step order");
    assert_eq!(rig.bytes(into, 0, 4096), data, "the read saw the write");
    let sent: Vec<_> = rig.io().since(mark).collect();
    let kinds: Vec<Kind> = sent.iter().map(|s| s.describe().0).collect();
    assert_eq!(kinds, [Kind::Write, Kind::Sync, Kind::Read], "{sent:?}");
    let chain = sent[0].chain().map(|(c, _, len)| (c, len));
    for (i, s) in sent.iter().enumerate() {
        if s.chain() != chain.map(|(c, len)| (c, i as u32, len)) {
            fail(format_args!("not one chain of three in order: {sent:?}"));
        }
    }
    rig.release(from);
    rig.release(into);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// A status of a missing path ends the sequence: the steps after it are
/// cancelled and not performed, the ones before it are done.
fn a_failure_ends_the_rest<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let p = format!("{root}ended.dat");
    let missing = format!("{root}missing.dat");
    let f = rig.open(&p, rw_create().with_empty(true));
    let e = rig.extent();
    rig.fill(e, 0, &bytes(1, 100));
    let steps = [
        write_step(f, e, 100, 0),
        Step::Status { path: &missing },
        write_step(f, e, 100, 50_000),
    ];
    let (got, ordered) = results(&mut rig, &steps);
    let cancelled = Err(FileError::Cancelled);
    assert_eq!(got, [Ok(100), Err(FileError::NotFound), cancelled]);
    assert!(ordered, "events in step order");
    assert_eq!(rig.status(&p), Ok(100), "the cancelled write was not done");
    let first = [
        Step::Status { path: &missing },
        Step::Sync {
            file: f,
            data_only: false,
        },
    ];
    let (early, _) = results(&mut rig, &first);
    assert_eq!(early, [Err(FileError::NotFound), cancelled]);
    rig.release(e);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// A read that ends at the end of the file breaks the backend's chain;
/// the set ends it there and goes on with the rest, which succeed.
fn a_short_read_is_not_a_failure<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}short.dat"), rw_create().with_empty(true));
    let head = bytes(2, 100);
    assert_eq!(rig.write(f, &head, 0), Ok(100));
    let (into, from) = (rig.extent(), rig.extent());
    let tail = bytes(3, 900);
    rig.fill(from, 0, &tail);
    let steps = [
        read_step(f, into, 4096, 0),
        write_step(f, from, 900, 100),
        Step::Sync {
            file: f,
            data_only: true,
        },
    ];
    let (got, ordered) = results(&mut rig, &steps);
    assert_eq!(got, [Ok(100), Ok(900), Ok(0)]);
    assert!(ordered, "events in step order");
    assert_eq!(rig.bytes(into, 0, 100), head);
    let mut whole = head;
    whole.extend_from_slice(&tail);
    assert_eq!(rig.read(f, 0, 1000), (Ok(1000), whole));
    rig.release(into);
    rig.release(from);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// Writes of two files and a rename, as one sequence.
fn steps_of_several_files_and_a_path<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let (a, b) = (format!("{root}two-a.dat"), format!("{root}two-b.dat"));
    let moved = format!("{root}two-moved.dat");
    let f = rig.open(&a, rw_create().with_empty(true));
    let g = rig.open(&b, rw_create().with_empty(true));
    let e = rig.extent();
    rig.fill(e, 0, &bytes(4, 2000));
    let steps = [
        write_step(f, e, 2000, 0),
        write_step(g, e, 1000, 0),
        Step::Rename {
            from: &a,
            to: &moved,
        },
    ];
    let (got, ordered) = results(&mut rig, &steps);
    assert_eq!(got, [Ok(2000), Ok(1000), Ok(0)]);
    assert!(ordered, "events in step order");
    assert_eq!(rig.status(&moved), Ok(2000));
    assert_eq!(rig.status(&b), Ok(1000));
    rig.release(e);
    assert_eq!(rig.close(f), Ok(0));
    assert_eq!(rig.close(g), Ok(0));
    rig.at_rest(0);
}

/// The chain ends at a path step: the steps behind it wait, not given to
/// the backend, until it has reported success, and go then as a chain of
/// their own (a backend need not end a chain at a failed path operation:
/// the ring does not).
fn steps_behind_a_path_step_wait_for_it<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let p = format!("{root}behind.dat");
    let f = rig.open(&p, rw_create().with_empty(true));
    let e = rig.extent();
    rig.fill(e, 0, &bytes(8, 512));
    let steps = [
        Step::Status { path: &p },
        write_step(f, e, 512, 0),
        Step::Sync {
            file: f,
            data_only: true,
        },
    ];
    let mark = rig.io().submitted();
    let mut ts = [Ticket::NONE; 3];
    let n = rig.call(|s, io, x| s.sequence(io, x, &steps, &mut ts));
    assert_eq!(n, Ok(3));
    assert_eq!(rig.io().submitted(), mark + 1, "only the status went");
    assert_eq!(rig.usage().waiting(), 2, "the steps behind it wait");
    let got = ts.map(|t| rig.done(t));
    assert_eq!(got, [Ok(0), Ok(512), Ok(0)]);
    let sent: Vec<_> = rig.io().since(mark).collect();
    let kinds: Vec<Kind> = sent.iter().map(|s| s.describe().0).collect();
    assert_eq!(kinds, [Kind::Status, Kind::Write, Kind::Sync], "{sent:?}");
    assert!(
        sent[1].chain().is_some() && sent[1].chain().map(|c| c.0) == sent[2].chain().map(|c| c.0)
    );
    rig.release(e);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
