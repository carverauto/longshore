// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The per-file bound and the slots (C2, C4): one file at its cap is
//! refused `Full` while every other open file can still start an
//! operation, path operations have slots of their own, a sequence needs
//! room for every step, close is never `Full`, and work that waits for
//! room at the backend goes first in, first out, a sequence not overtaken.

use super::{limits, must, rw_create, Kind, Rig, Submitted};
use crate::{File, FileError, FileSizes, Step, Ticket};
use longshore_common::io::{Io, Limits};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    one_file_at_its_cap(make, root);
    the_cap_is_per_file(make, root);
    a_sequence_needs_room_for_every_step(make, root);
    waiting_goes_first_in_first_out(make, root);
}

/// Three files of three operations each, two shared slots, one path slot.
fn three() -> FileSizes {
    FileSizes::new(3, 3)
        .with_shared(2)
        .with_paths(1)
        .with_sequence_max(3)
        .with_spans_max(4)
}

fn sync<IO: Io>(rig: &mut Rig<IO>, f: File) -> Result<Ticket, FileError> {
    rig.call(|s, io, _| s.sync(io, f, true))
}

/// One file takes its own slot and both shared ones, and is `Full`; the
/// second file still starts one, then finds no shared slot; the third
/// starts one; a path operation has its own slot, and a second is `Full`;
/// the busy file's close is taken and reported after its operations.
fn one_file_at_its_cap<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), three());
    let [a, b, c] = ["a", "b", "c"].map(|n| rig.open(&format!("{root}cap-{n}.dat"), rw_create()));
    let mut busy = Vec::new();
    for _ in 0..3 {
        busy.push(must(sync(&mut rig, a), "a's own slot and the shared ones"));
    }
    let full = FileError::Full;
    rig.refused("a at its cap", full, |s, io, _| s.sync(io, a, true));
    // A write refused for want of room leaves its extent's holds alone.
    let spare = rig.extent();
    rig.refused("a write at its cap", full, |s, io, x| {
        s.write_at(io, x, a, (spare, 0, 1), 0)
    });
    rig.release(spare);
    let of_b = must(sync(&mut rig, b), "b's own slot");
    rig.refused("b, no shared slot", full, |s, io, _| s.sync(io, b, true));
    let of_c = must(sync(&mut rig, c), "c's own slot");
    let p = format!("{root}cap-a.dat");
    let path = must(rig.call(|s, io, _| s.status(io, &p)), "the path slot");
    rig.refused("a second path", full, |s, io, _| s.status(io, &p));
    assert_eq!(rig.usage().ops(), 6);
    let close = must(rig.call(|s, io, _| s.close(io, a)), "close is never Full");
    let mut last = 0;
    for t in busy {
        let (seq, e) = rig.wait(t);
        assert_eq!(e, super::basics::done(t, Some(a), Ok(0)));
        last = last.max(seq);
    }
    let (closed_at, closed) = rig.wait(close);
    assert_eq!(closed, super::basics::done(close, Some(a), Ok(0)));
    assert!(
        closed_at > last,
        "the close is reported after a's operations"
    );
    assert_eq!(rig.done(of_b), Ok(0));
    assert_eq!(rig.done(of_c), Ok(0));
    assert_eq!(rig.done(path), Ok(0));
    rig.at_rest(2);
    assert_eq!(rig.close(b), Ok(0));
    assert_eq!(rig.close(c), Ok(0));
    rig.at_rest(0);
}

/// With shared slots to spare, a file at its cap is `Full` all the same,
/// and another file still starts as many as its own cap.
fn the_cap_is_per_file<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let roomy = FileSizes::new(2, 2)
        .with_shared(4)
        .with_paths(1)
        .with_sequence_max(2)
        .with_spans_max(4);
    let mut rig = Rig::new(make, limits(), roomy);
    let a = rig.open(&format!("{root}per-file-a.dat"), rw_create());
    let b = rig.open(&format!("{root}per-file-b.dat"), rw_create());
    let mut ts = Vec::new();
    for f in [a, a, b, b] {
        ts.push(must(sync(&mut rig, f), "within the cap"));
    }
    rig.refused("a at its cap", FileError::Full, |s, io, _| {
        s.sync(io, a, true)
    });
    rig.refused("b at its cap", FileError::Full, |s, io, _| {
        s.sync(io, b, true)
    });
    for t in ts {
        assert_eq!(rig.done(t), Ok(0));
    }
    assert_eq!(rig.close(a), Ok(0));
    assert_eq!(rig.close(b), Ok(0));
    rig.at_rest(0);
}

/// With two of a file's three slots in use, a sequence of two of its steps
/// is `Full` and takes nothing; one of one step is taken.
fn a_sequence_needs_room_for_every_step<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), three());
    let a = rig.open(&format!("{root}room.dat"), rw_create());
    let first = [
        must(sync(&mut rig, a), "one"),
        must(sync(&mut rig, a), "two"),
    ];
    let step = Step::Sync {
        file: a,
        data_only: true,
    };
    let (two, one) = ([step; 2], [step]);
    let mut ts = [Ticket::NONE; 2];
    rig.refused("two steps", FileError::Full, |s, io, x| {
        s.sequence(io, x, &two, &mut ts)
    });
    let n = rig.call(|s, io, x| s.sequence(io, x, &one, &mut ts));
    assert_eq!(n, Ok(1));
    for t in first.into_iter().chain([ts[0]]) {
        assert_eq!(rig.done(t), Ok(0));
    }
    assert_eq!(rig.close(a), Ok(0));
    rig.at_rest(0);
}

/// What the backend has no room for waits, and goes when room comes back
/// in the order it was asked for: a sequence that needs more room than
/// there is holds back what was asked for after it.
fn waiting_goes_first_in_first_out<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), three().with_shared(4));
    let a = rig.open(&format!("{root}wait-a.dat"), rw_create());
    let b = rig.open(&format!("{root}wait-b.dat"), rw_create());
    let e = rig.extent();
    let mark = rig.io().submitted();
    rig.io().set_room(Some(0));
    let write = |r: &mut Rig<IO>, f: File, off: u64| {
        must(
            r.call(|s, io, x| s.write_at(io, x, f, (e, 0, 512), off)),
            "a write that waits",
        )
    };
    let mut ts = vec![write(&mut rig, a, 0), write(&mut rig, b, 0)];
    let steps = [
        Step::Write {
            file: a,
            extent: e,
            at: 0,
            len: 512,
            file_off: 4096,
        },
        Step::Sync {
            file: a,
            data_only: true,
        },
    ];
    let mut seq = [Ticket::NONE; 2];
    let n = rig.call(|s, io, x| s.sequence(io, x, &steps, &mut seq));
    assert_eq!(n, Ok(2), "a sequence that waits");
    ts.extend_from_slice(&seq);
    ts.push(write(&mut rig, b, 4096));
    assert_eq!(rig.io().submitted(), mark, "nothing went without room");
    assert_eq!(rig.usage().waiting(), 5);
    let poll = |r: &mut Rig<IO>, room: u32| {
        r.io().set_room(Some(room));
        r.call(|s, io, x| s.poll(io, x));
    };
    poll(&mut rig, 1);
    poll(&mut rig, 1);
    poll(&mut rig, 1);
    assert_eq!(
        rig.io().submitted(),
        mark + 2,
        "the sequence is not overtaken"
    );
    poll(&mut rig, 2);
    poll(&mut rig, 1);
    rig.io().set_room(None);
    let sent: Vec<Submitted> = rig.io().since(mark).collect();
    let seen: Vec<_> = sent.iter().map(|s| s.describe()).collect();
    let want = [
        (Kind::Write, Some(0)),
        (Kind::Write, Some(0)),
        (Kind::Write, Some(4096)),
        (Kind::Sync, None),
        (Kind::Write, Some(4096)),
    ];
    let kinds: Vec<_> = seen.iter().map(|(k, _, off)| (*k, *off)).collect();
    assert_eq!(kinds, want, "first in, first out: {sent:?}");
    let slots: Vec<_> = seen.iter().map(|(_, slot, _)| *slot).collect();
    assert!(slots[0] == slots[2] && slots[2] == slots[3] && slots[1] == slots[4]);
    assert!(slots[0] != slots[1], "{slots:?}");
    for t in ts {
        let r = rig.done(t);
        assert!(r == Ok(512) || r == Ok(0), "{r:?}");
    }
    rig.release(e);
    assert_eq!(rig.close(a), Ok(0));
    assert_eq!(rig.close(b), Ok(0));
    rig.at_rest(0);
}
