// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Faults of the simulated disk: a short write resumed, a failed sync
//! that poisons, a sequence ended by a failure, power loss (contract C6,
//! C7, C10, C12).

use crate::rig::{disk, pattern, rw, Rig};
use longshore_common::io::extents::Extent;
use longshore_common::io::{Io, OpenFlags};
use longshore_common::sim::{DiskFaults, Faults, PlacedFault, SimIo};
use longshore_file::{ErrorKind, File, FileError, FileSizes, Step, Ticket};

/// Reads `len` bytes of `f` from `off` and checks they are `src`'s first
/// `len` bytes.
fn read_back(r: &mut Rig, f: File, src: Extent, len: u32, off: u64) {
    let back = r.extent(0);
    assert_eq!(r.read(f, back, len, off), Ok(u64::from(len)));
    let want = r.io.buf(src.buf())[..len as usize].to_vec();
    assert!(
        r.io.buf(back.buf())[..len as usize] == want[..],
        "the bytes read back"
    );
    assert_eq!(r.x.release(back), Ok(true));
}

#[test]
fn a_short_write_is_resumed_to_the_whole() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/a", rw());
    // File operation 2 is the write: cut at a sector.
    r.io.place_disk_fault(2, PlacedFault::ShortWrite);
    let e = r.extent(0);
    pattern(&mut r, e, 0x42);
    assert_eq!(r.write(f, e, 12_288, 0), Ok(12_288));
    assert_eq!(
        r.io.disk_fault_counters().short_write(),
        1,
        "the fault fired"
    );
    read_back(&mut r, f, e, 12_288, 0);
}

#[test]
fn short_writes_at_any_rate_still_end_whole() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/a", rw().with_direct(true));
    r.io.set_disk_faults(DiskFaults::NONE.with_short_write(500_000));
    let e = r.extent(0);
    pattern(&mut r, e, 0x17);
    for k in 0..8 {
        assert_eq!(r.write(f, e, 16_384, k * 16_384), Ok(16_384));
    }
    r.io.set_disk_faults(DiskFaults::NONE);
    assert!(r.io.disk_fault_counters().short_write() > 0, "faults fired");
    for k in 0..8 {
        read_back(&mut r, f, e, 16_384, k * 16_384);
    }
}

#[test]
fn a_short_direct_write_below_a_block_is_written_again() {
    // 512-byte sectors: a short write can stop inside a block.
    for seed in 0..20u64 {
        let mut r = Rig::new(FileSizes::new(1, 1));
        *r.io = SimIo::new(r.limits, seed, Faults::NONE);
        r.io.setup_disk(disk(512));
        let f = r.open("/d", rw().with_direct(true));
        let e = r.extent(0);
        pattern(&mut r, e, 0x5C);
        r.io.place_disk_fault(2, PlacedFault::ShortWrite);
        assert_eq!(r.write(f, e, 8192, 0), Ok(8192), "seed {seed}");
        assert_eq!(r.io.disk_fault_counters().short_write(), 1);
        read_back(&mut r, f, e, 8192, 0);
    }
}

#[test]
fn a_direct_write_that_never_writes_a_block_ends() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    *r.io = SimIo::new(r.limits, 3, Faults::NONE);
    r.io.setup_disk(disk(512));
    let f = r.open("/d", rw().with_direct(true));
    let e = r.extent(1);
    // Every write short: some stop inside the first block each time.
    r.io.set_disk_faults(DiskFaults::NONE.with_short_write(1_000_000));
    let mut ended = 0;
    for k in 0..20 {
        match r.write(f, e, 8192, k * 8192) {
            Ok(8192) => {}
            Err(
                FileError::NoProgress
                | FileError::Partial {
                    kind: ErrorKind::NoProgress,
                    ..
                },
            ) => {
                ended += 1;
            }
            other => panic!("{other:?}"),
        }
    }
    let writes =
        r.io.submitted()
            .iter()
            .filter(|(_, _, op)| matches!(op, longshore_common::io::Op::FileWrite { .. }))
            .count();
    // Every transfer ended, and the ones that ended were written again
    // before they gave up.
    assert!(ended > 0, "no write ever stalled");
    assert!(writes >= 20 + 4, "{writes} writes for 20 transfers");
}

#[test]
fn an_error_part_way_reports_what_was_written() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/a", rw());
    r.io.place_disk_fault(2, PlacedFault::ShortWrite);
    r.io.place_disk_fault(3, PlacedFault::LostReported);
    let e = r.extent(1);
    match r.write(f, e, 12_288, 0) {
        Err(FileError::Partial { done, kind }) => {
            assert!(done > 0 && done < 12_288);
            assert_eq!(kind, ErrorKind::Io { errno: 5 });
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failed_sync_poisons_the_file() {
    let mut r = Rig::new(FileSizes::new(1, 2));
    let f = r.open("/a", rw());
    let e = r.extent(1);
    assert_eq!(r.write(f, e, 4096, 0), Ok(4096));
    r.io.place_disk_fault(3, PlacedFault::Fail);
    assert_eq!(r.sync(f), Err(FileError::SyncFailed { errno: 5 }));
    // Only reads and the close are taken now.
    let w = r.files.write_at(&mut r.io, &mut r.x, f, (e, 0, 4096), 0);
    assert_eq!(w, Err(FileError::Poisoned));
    assert_eq!(r.files.sync(&mut r.io, f, true), Err(FileError::Poisoned));
    read_back(&mut r, f, e, 4096, 0);
    r.close(f);
    // Opened again, the file is a new one.
    let g = r.open("/a", rw());
    assert_eq!(r.sync(g), Ok(0));
}

#[test]
fn a_failed_durable_write_is_a_failed_sync() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/a", rw().with_dsync(true));
    r.io.place_disk_fault(2, PlacedFault::Fail);
    let e = r.extent(1);
    assert_eq!(
        r.write(f, e, 4096, 0),
        Err(FileError::SyncFailed { errno: 5 })
    );
    let w = r.files.write_at(&mut r.io, &mut r.x, f, (e, 0, 4096), 0);
    assert_eq!(w, Err(FileError::Poisoned));
}

#[test]
fn a_failed_step_cancels_the_rest_of_its_sequence() {
    let mut r = Rig::new(FileSizes::new(1, 3).with_shared(2));
    let f = r.open("/a", rw());
    let e = r.extent(1);
    r.io.place_disk_fault(3, PlacedFault::Fail);
    let steps = [
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 4096,
            file_off: 0,
        },
        Step::Sync {
            file: f,
            data_only: false,
        },
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 4096,
            file_off: 4096,
        },
    ];
    let mut t = [Ticket::NONE; 3];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(3));
    assert_eq!(r.wait(t[0]).1, Ok(4096));
    assert_eq!(r.wait(t[1]).1, Err(FileError::SyncFailed { errno: 5 }));
    assert_eq!(r.wait(t[2]).1, Err(FileError::Cancelled));
    assert_eq!(r.x.holds(e), Some(1));
}

#[test]
fn a_short_step_resumes_and_the_rest_follow() {
    let mut r = Rig::new(FileSizes::new(1, 2));
    let f = r.open("/a", rw());
    r.io.place_disk_fault(2, PlacedFault::ShortWrite);
    let e = r.extent(0);
    pattern(&mut r, e, 3);
    let steps = [
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 12_288,
            file_off: 0,
        },
        Step::Sync {
            file: f,
            data_only: true,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(2));
    assert_eq!(r.wait(t[0]).1, Ok(12_288));
    assert_eq!(r.wait(t[1]).1, Ok(0));
    assert_eq!(
        r.io.disk_fault_counters().short_write(),
        1,
        "the fault fired"
    );
    read_back(&mut r, f, e, 12_288, 0);
}

#[test]
fn power_lost_frees_everything_and_keeps_what_was_synced() {
    let mut r = Rig::new(FileSizes::new(2, 2));
    let f = r.open("/a", rw());
    let e = r.extent(0);
    pattern(&mut r, e, 0x33);
    assert_eq!(r.write(f, e, 4096, 0), Ok(4096));
    assert_eq!(r.sync(f), Ok(0));
    // The file's name is durable once its directory is synced.
    let dir = r.open("/", OpenFlags::NONE.with_directory(true));
    assert_eq!(r.sync(dir), Ok(0));
    let late = r
        .files
        .write_at(&mut r.io, &mut r.x, f, (e, 0, 4096), 4096)
        .unwrap();
    assert_eq!(r.x.holds(e), Some(2));
    r.io.power_loss();
    r.files.power_lost(&mut r.x);
    assert_eq!(r.x.holds(e), Some(1), "the set's hold is released");
    let u = r.files.usage();
    assert_eq!((u.files(), u.ops(), u.waiting()), (0, 0, 0));
    for _ in 0..10 {
        r.turn();
    }
    assert!(r.events.iter().all(|ev| crate::rig::ticket_of(ev) != late));
    // The old handle is stale; the file opens again with exactly the bytes
    // synced: the write in flight was never run.
    assert_eq!(r.files.sync(&mut r.io, f, true), Err(FileError::Stale));
    let g = r.open("/a", rw());
    let t = r.files.status(&mut r.io, "/a").unwrap();
    assert_eq!(r.wait(t).1, Ok(4096));
    read_back(&mut r, g, e, 4096, 0);
}
