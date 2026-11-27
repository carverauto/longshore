// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Poisoning and errors (C10), on the simulator, whose disk can be made to
//! fail: a failed sync and a failed durable write poison the file, which
//! then takes reads and its close only; a failed buffered write does not;
//! waiting work of a poisoned file ends `Poisoned`, a transfer in progress
//! `Partial { kind: Poisoned }`; the disk's errors map to one type.

use super::sim::{place, rig};
use longshore_common::sim::{DiskFaults, PlacedFault};
use longshore_file::check::{bytes, must, rw_create, tickets};
use longshore_file::{ErrorKind, FileError, Span, Step};

const EIO: i32 = 5;

#[test]
fn a_failed_sync_poisons_the_file_and_reads_and_its_close_go_on() {
    for fault in [PlacedFault::Fail, PlacedFault::FailAfter] {
        let mut rig = rig();
        let f = rig.open("/poison.dat", rw_create());
        let data = bytes(1, 1000);
        assert_eq!(rig.write(f, &data, 0), Ok(1000));
        place(&mut rig, 1, fault);
        assert_eq!(rig.sync(f, true), Err(FileError::SyncFailed { errno: EIO }));
        let e = rig.extent();
        let p = FileError::Poisoned;
        rig.refused("write", p, |s, io, x| s.write_at(io, x, f, (e, 0, 1), 0));
        let span = [Span::Extent {
            extent: e,
            at: 0,
            len: 1,
        }];
        rig.refused("spans", p, |s, io, x| s.write_spans(io, x, f, &span, 0));
        rig.refused("sync", p, |s, io, _| s.sync(io, f, false));
        rig.refused("reserve", p, |s, io, _| s.reserve(io, f, (0, 10), false));
        rig.refused("punch", p, |s, io, _| s.punch(io, f, (0, 4096)));
        let steps = [Step::Sync {
            file: f,
            data_only: true,
        }];
        let mut ts = tickets::<1>();
        rig.refused("a step", p, |s, io, x| s.sequence(io, x, &steps, &mut ts));
        rig.release(e);
        assert_eq!(rig.read(f, 0, 1000), (Ok(1000), data));
        assert_eq!(rig.close(f), Ok(0));
        // Opened again, it is a file like any other.
        let g = rig.open("/poison.dat", rw_create());
        assert_eq!(rig.write(g, b"x", 0), Ok(1));
        assert_eq!(rig.sync(g, true), Ok(0));
        assert_eq!(rig.close(g), Ok(0));
        rig.at_rest(0);
    }
}

#[test]
fn a_failed_durable_write_poisons_and_a_failed_buffered_one_does_not() {
    let mut rig = rig();
    let durable = rig.open("/dsync.dat", rw_create().with_dsync(true));
    place(&mut rig, 1, PlacedFault::Fail);
    // EIO on a durable write is a failed sync (C10).
    let r = rig.write(durable, &[1; 100], 0);
    assert_eq!(r, Err(FileError::SyncFailed { errno: EIO }));
    let e = rig.extent();
    rig.refused("after", FileError::Poisoned, |s, io, x| {
        s.write_at(io, x, durable, (e, 0, 1), 0)
    });
    rig.release(e);
    assert_eq!(rig.close(durable), Ok(0));

    let buffered = rig.open("/buffered.dat", rw_create());
    place(&mut rig, 1, PlacedFault::Fail);
    assert_eq!(
        rig.write(buffered, &[2; 100], 0),
        Err(FileError::Io { errno: EIO })
    );
    assert_eq!(rig.write(buffered, &[3; 100], 0), Ok(100));
    assert_eq!(rig.sync(buffered, false), Ok(0));
    assert_eq!(rig.close(buffered), Ok(0));
    rig.at_rest(0);
}

#[test]
fn waiting_work_of_a_poisoned_file_ends_poisoned_unsubmitted() {
    let mut rig = rig();
    let f = rig.open("/waiting.dat", rw_create());
    place(&mut rig, 1, PlacedFault::Fail);
    let y = must(rig.call(|s, io, _| s.sync(io, f, true)), "sync");
    rig.io().set_room(Some(0));
    let e = rig.extent();
    let w = must(
        rig.call(|s, io, x| s.write_at(io, x, f, (e, 0, 100), 0)),
        "a write that waits",
    );
    rig.release(e);
    let mark = rig.io().submitted();
    assert_eq!(rig.done(y), Err(FileError::SyncFailed { errno: EIO }));
    assert_eq!(rig.done(w), Err(FileError::Poisoned));
    assert_eq!(rig.io().submitted(), mark, "the waiting write never went");
    assert!(!rig.x().is_live(e), "its extent was let go of");
    rig.io().set_room(None);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

#[test]
fn a_transfer_in_progress_ends_partial_poisoned() {
    let mut rig = rig();
    let f = rig.open("/progress.dat", rw_create());
    place(&mut rig, 1, PlacedFault::Fail);
    place(&mut rig, 2, PlacedFault::ShortWrite);
    let y = must(rig.call(|s, io, _| s.sync(io, f, true)), "sync");
    let e = rig.extent();
    rig.fill(e, 0, &bytes(2, 2048));
    let w = must(
        rig.call(|s, io, x| s.write_at(io, x, f, (e, 0, 2048), 0)),
        "write",
    );
    rig.release(e);
    // The rest of the write cannot go before the sync's failure is seen.
    rig.io().set_room(Some(0));
    assert_eq!(rig.done(y), Err(FileError::SyncFailed { errno: EIO }));
    match rig.done(w) {
        Err(FileError::Partial {
            done,
            kind: ErrorKind::Poisoned,
        }) => assert!(
            done > 0 && done < 2048 && done.is_multiple_of(512),
            "{done}"
        ),
        other => panic!("the cut write: {other:?}"),
    }
    rig.io().set_room(None);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

#[test]
fn the_disks_errors_map_to_one_type() {
    let mut rig = rig();
    let f = rig.open("/errors.dat", rw_create());
    assert_eq!(rig.write(f, &[7; 8192], 0), Ok(8192));
    let refused = DiskFaults::NONE.with_reserve_refused(1_000_000);
    rig.io().inner_mut().set_disk_faults(refused);
    let reserved = rig.run("reserve", |s, io, _| s.reserve(io, f, (8192, 4096), false));
    assert_eq!(reserved, Err(FileError::NoSpace));
    let no_punch = DiskFaults::NONE.with_punch_refused(1_000_000);
    rig.io().inner_mut().set_disk_faults(no_punch);
    let punched = rig.run("punch", |s, io, _| s.punch(io, f, (0, 4096)));
    assert_eq!(punched, Err(FileError::Unsupported));
    rig.io().inner_mut().set_disk_faults(DiskFaults::NONE);
    place(&mut rig, 1, PlacedFault::Fail);
    assert_eq!(rig.read(f, 0, 100).0, Err(FileError::Io { errno: EIO }));
    let bad = DiskFaults::NONE.with_read_error(1_000_000);
    rig.io().inner_mut().set_disk_faults(bad);
    assert_eq!(rig.read(f, 0, 100).0, Err(FileError::Io { errno: EIO }));
    rig.io().inner_mut().set_disk_faults(DiskFaults::NONE);
    // None of them poisons the file.
    assert_eq!(rig.write(f, &[8; 4096], 0), Ok(4096));
    assert_eq!(rig.sync(f, false), Ok(0));
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
