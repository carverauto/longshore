// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A sequence's chain at the backend: a step of a poisoned file behind
//! it ends only once the steps before it are submitted (contract C4, C7),
//! and a resumed step whose chain the backend refuses keeps the bytes it
//! moved (C6).

use crate::record::Entry;
use crate::rig::{disk, rw, Rig};
use longshore_common::io::{Completion, Io, Limits, Token};
use longshore_common::sim::{Faults, PlacedFault, SimIo};
use longshore_file::{ErrorKind, FileError, FileSizes, Span, Step, Ticket};

/// The rig's backend replaced by one built with `l`.
fn backend(r: &mut Rig, l: Limits) {
    *r.io = SimIo::new(l, 7, Faults::NONE);
    r.io.setup_disk(disk(4096));
}

fn token_of(r: &Rig, t: Ticket) -> Token {
    Token::new(r.files.config().token_base() + u64::from(t.index()))
}

/// The position in the backend's log where `t` was last submitted (a
/// file slot's own record is its open's and then its close's).
fn submitted_at(r: &Rig, t: Token) -> usize {
    let at = r.io.log.iter().rposition(|e| match e {
        Entry::Op(k, _) => *k == t,
        Entry::Chain(ops) => ops.iter().any(|(k, _)| *k == t),
        Entry::Done(..) => false,
    });
    at.unwrap_or_else(|| panic!("{t:?} never submitted"))
}

/// Ticks until `n` completions have come, without handing them to the set.
fn collect(r: &mut Rig, n: usize) -> Vec<Completion> {
    let mut got = Vec::new();
    for _ in 0..100 {
        r.io.tick(false, &mut r.out).unwrap();
        while let Some(c) = r.out.pop() {
            got.push(c);
        }
        if got.len() == n {
            return got;
        }
    }
    panic!("{} of {n} completions came", got.len());
}

#[test]
fn a_held_close_does_not_overtake_the_sequence_whose_step_it_waited_for() {
    // A backend with room for one operation.
    let mut r = Rig::new(FileSizes::new(2, 2));
    let one = r.limits.with_ops_max(1);
    backend(&mut r, one);
    let (a, b) = (r.open("/a", rw()), r.open("/b", rw()));
    let (ea, eb) = (r.extent(0x0A), r.extent(0x0B));
    // b's sync fails, poisoning b, while the sequence waits for room.
    r.io.place_disk_fault(3, PlacedFault::Fail);
    let s = r.files.sync(&mut r.io, b, false).unwrap();
    let steps = [
        Step::Write {
            file: a,
            extent: ea,
            at: 0,
            len: 4096,
            file_off: 0,
        },
        Step::Write {
            file: b,
            extent: eb,
            at: 0,
            len: 4096,
            file_off: 0,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(2));
    assert_eq!(r.files.usage().waiting(), 2);
    // b's close waits for the sequence's step of b.
    let c = r.files.close(&mut r.io, b).unwrap();
    assert_eq!(r.wait(s).1, Err(FileError::SyncFailed { errno: 5 }));
    assert_eq!(r.wait(t[1]).1, Err(FileError::Poisoned));
    assert_eq!(r.wait(t[0]).1, Ok(4096));
    assert_eq!(r.wait(c).1, Ok(0));
    // The sequence waited for room first: the close goes after it.
    let (seq, close) = (token_of(&r, t[0]), token_of(&r, c));
    assert!(
        submitted_at(&r, seq) < submitted_at(&r, close),
        "the close overtook the sequence waiting before it"
    );
}

#[test]
fn a_resumed_step_whose_chain_the_backend_refuses_reports_its_bytes() {
    // A backend with one gather row.
    let mut r = Rig::new(FileSizes::new(1, 3).with_shared(2));
    let l = r.limits.with_gather(1, r.limits.gather_spans());
    backend(&mut r, l);
    let f = r.open("/a", rw());
    let e = r.extent(0x5A);
    let sp = [Span::Extent {
        extent: e,
        at: 0,
        len: 4096,
    }];
    // The write is cut short: the chain breaks, the gathered write not run.
    r.io.place_disk_fault(2, PlacedFault::ShortWrite);
    let steps = [
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 8192,
            file_off: 0,
        },
        Step::WriteSpans {
            file: f,
            spans: &sp,
            file_off: 8192,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(2));
    let got = collect(&mut r, 2);
    let cut = got
        .iter()
        .find(|c| c.token() == token_of(&r, t[0]))
        .map_or(0, |c| c.result());
    assert!(cut > 0 && cut < 8192, "the write was cut short: {cut}");
    // Another write takes the gather row before the chain is resumed.
    let w = r
        .files
        .write_spans(&mut r.io, &mut r.x, f, &sp, 16_384)
        .unwrap();
    for c in got {
        assert!(r.files.on_completion(&mut r.io, &mut r.x, c));
    }
    let done = u64::from(cut.unsigned_abs());
    let partial = FileError::Partial {
        done,
        kind: ErrorKind::Os { errno: 125 },
    };
    assert_eq!(r.wait(t[0]).1, Err(partial), "the bytes the write moved");
    assert_eq!(r.wait(t[1]).1, Err(FileError::Os { errno: 105 }));
    assert_eq!(r.wait(w).1, Ok(4096));
}
