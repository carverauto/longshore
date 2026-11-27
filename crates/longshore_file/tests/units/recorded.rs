// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What reached the backend, and when, by its own record (contract C3,
//! C11, F1).

use crate::record::Entry;
use crate::rig::{rw, Rig};
use longshore_common::io::{Op, OpenFlags, Token};
use longshore_file::{FileError, FileSizes, Span, Step, Ticket};

#[test]
fn a_close_goes_only_once_the_files_other_operations_completed() {
    let mut r = Rig::new(FileSizes::new(2, 3).with_shared(2));
    let f = r.open("/a", rw());
    let e = r.extent(1);
    let w = r
        .files
        .write_at(&mut r.io, &mut r.x, f, (e, 0, 8192), 0)
        .unwrap();
    let s = r.files.sync(&mut r.io, f, false).unwrap();
    let rd = r
        .files
        .read_at(&mut r.io, &mut r.x, f, (e, 0, 4096), 0)
        .unwrap();
    let c = r.files.close(&mut r.io, f).unwrap();
    for t in [w, s, rd, c] {
        assert!(r.wait(t).1.is_ok());
    }
    let subs = r.io.submitted();
    let close_at = subs
        .iter()
        .find(|(_, _, op)| matches!(op, Op::FileClose { .. }))
        .map(|(k, _, _)| *k)
        .unwrap();
    let others: Vec<_> = subs
        .iter()
        .filter(|(_, _, op)| {
            matches!(
                op,
                Op::FileWrite { .. } | Op::FileSync { .. } | Op::FileRead { .. }
            )
        })
        .collect();
    assert_eq!(others.len(), 3);
    for (k, t, op) in others {
        let (done, res) = r.io.done_at(*t, *k).unwrap();
        assert!(res >= 0, "{op:?}");
        assert!(
            done < close_at,
            "{op:?} completed after the close was submitted"
        );
    }
}

#[test]
fn a_refused_call_submits_nothing() {
    let mut r = Rig::new(FileSizes::new(3, 2).with_paths(1));
    let f = r.open("/a", rw());
    let d = r.open("/d", rw().with_direct(true));
    let ro = r.open("/a", OpenFlags::NONE.with_read(true));
    let e = r.extent(1);
    let gone = r.extent(1);
    assert_eq!(r.x.release(gone), Ok(true));
    let before = r.io.log.len();
    let x = &mut r.x;
    let io = &mut r.io;
    let fs = &mut r.files;
    let refused = [
        fs.write_at(io, x, d, (e, 0, 100), 0),
        fs.write_at(io, x, ro, (e, 0, 100), 0),
        fs.read_at(io, x, f, (gone, 0, 100), 0),
        fs.read_at(io, x, f, (e, 1, u32::MAX), 0),
        fs.punch(io, f, (0, 100)),
        fs.reserve(io, f, (0, 0), false),
        fs.unlink(io, "/a\0b"),
        fs.write_spans(io, x, f, &[], 0),
        fs.write_spans(
            io,
            x,
            d,
            &[Span::Extent {
                extent: e,
                at: 0,
                len: 10,
            }],
            0,
        ),
    ];
    for res in refused {
        assert!(res.is_err(), "{res:?}");
    }
    // A sequence refused whole, at its second step.
    let steps = [
        Step::Sync {
            file: f,
            data_only: true,
        },
        Step::Write {
            file: d,
            extent: e,
            at: 0,
            len: 7,
            file_off: 0,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    let seq = r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t);
    assert_eq!(seq, Err(FileError::Unaligned));
    // Full: the file's two slots taken, then asked again.
    let held = [
        r.files.sync(&mut r.io, f, true).unwrap(),
        r.files.sync(&mut r.io, f, true).unwrap(),
    ];
    let after_held = r.io.log.len();
    assert_eq!(r.files.sync(&mut r.io, f, true), Err(FileError::Full));
    assert_eq!(r.io.log.len(), after_held, "a Full refusal submits nothing");
    assert_eq!(after_held, before + 2, "only the accepted syncs went");
    assert!(matches!(
        r.io.log[before],
        Entry::Op(_, Op::FileSync { .. })
    ));
    assert_eq!(r.x.holds(e), Some(1), "no hold kept");
    for h in held {
        assert_eq!(r.wait(h).1, Ok(0));
    }
}

/// The backend token of ticket `t` of the rig's set.
fn token_of(r: &Rig, t: Ticket) -> Token {
    Token::new(r.files.config().token_base() + u64::from(t.index()))
}

/// A sequence of a write, a status of `path` and a write behind it.
fn write_status_write(r: &mut Rig, path: &str) -> [Ticket; 3] {
    let f = r.open("/a", rw());
    let e = r.extent(1);
    let steps = [
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 100,
            file_off: 0,
        },
        Step::Status { path },
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 100,
            file_off: 50_000,
        },
    ];
    let mut t = [Ticket::NONE; 3];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(3));
    t
}

#[test]
fn a_path_step_ends_its_chain_and_what_follows_waits_for_it() {
    let mut r = Rig::new(FileSizes::new(1, 3).with_shared(2));
    let t = write_status_write(&mut r, "/a");
    assert_eq!(r.wait(t[0]).1, Ok(100));
    assert_eq!(r.wait(t[1]).1, Ok(100));
    assert_eq!(r.wait(t[2]).1, Ok(100));
    let (status, last) = (token_of(&r, t[1]), token_of(&r, t[2]));
    let subs = r.io.submitted();
    let (k, _, _) = *subs.iter().find(|(_, tok, _)| *tok == status).unwrap();
    let (status_done, _) = r.io.done_at(status, k).unwrap();
    let (w, _, _) = *subs.iter().find(|(_, tok, _)| *tok == last).unwrap();
    assert!(w > status_done, "the write behind the status went with it");
}

#[test]
fn a_path_step_that_fails_keeps_what_follows_from_the_backend() {
    let mut r = Rig::new(FileSizes::new(1, 3).with_shared(2));
    let t = write_status_write(&mut r, "/missing");
    assert_eq!(r.wait(t[0]).1, Ok(100));
    assert_eq!(r.wait(t[1]).1, Err(FileError::NotFound));
    assert_eq!(r.wait(t[2]).1, Err(FileError::Cancelled));
    let last = token_of(&r, t[2]);
    assert!(r.io.submitted().iter().all(|(_, tok, _)| *tok != last));
    let s = r.files.status(&mut r.io, "/a").unwrap();
    assert_eq!(r.wait(s).1, Ok(100), "the last write never ran");
}
