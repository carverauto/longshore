// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! No file can take every slot, and work that finds the backend full
//! waits, first in first out (contract C2, C4).

use crate::rig::{rw, Rig};
use longshore_common::io::Limits;
use longshore_common::sim::{DiskConfig, Faults, SimIo};
use longshore_file::{FileError, FileSizes};

#[test]
fn a_busy_file_stops_at_its_cap_and_another_still_starts() {
    // Two files, two operations each, one shared slot.
    let mut r = Rig::new(FileSizes::new(2, 2).with_shared(1));
    let (a, b) = (r.open("/a", rw()), r.open("/b", rw()));
    let t1 = r.files.sync(&mut r.io, a, true).unwrap();
    let t2 = r.files.sync(&mut r.io, a, true).unwrap();
    assert_eq!(r.files.sync(&mut r.io, a, true), Err(FileError::Full));
    // The shared slot is a's now, but b has its own.
    let t3 = r.files.sync(&mut r.io, b, true).unwrap();
    assert_eq!(r.files.sync(&mut r.io, b, true), Err(FileError::Full));
    for t in [t1, t2, t3] {
        assert_eq!(r.wait(t).1, Ok(0));
    }
    // Every slot is back once the events are taken.
    let t4 = r.files.sync(&mut r.io, b, true).unwrap();
    let t5 = r.files.sync(&mut r.io, b, true).unwrap();
    assert_eq!(r.wait(t4).1, Ok(0));
    assert_eq!(r.wait(t5).1, Ok(0));
}

#[test]
fn a_file_slot_is_full_until_a_close_is_taken() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let a = r.open("/a", rw());
    assert_eq!(r.files.open(&mut r.io, "/b", rw()), Err(FileError::Full));
    // A close is never Full, even with the file's only slot in use.
    let s = r.files.sync(&mut r.io, a, true).unwrap();
    let c = r.files.close(&mut r.io, a).unwrap();
    assert_eq!(r.wait(s).1, Ok(0));
    assert_eq!(r.wait(c).1, Ok(0));
    let b = r.open("/b", rw());
    assert_ne!(a, b);
}

#[test]
fn work_waits_for_room_first_in_first_out() {
    // The backend has room for one operation, fewer than the set planned.
    let sizes = FileSizes::new(2, 2);
    let mut r = Rig::new(sizes);
    let small: Limits = r.limits.with_ops_max(1);
    r.io = SimIo::new(small, 7, Faults::NONE);
    r.io.setup_disk(
        DiskConfig::BASE
            .with_max_files(16)
            .with_max_dirs(8)
            .with_max_bytes_per_file(1 << 20)
            .with_max_name_len(256),
    );
    let a = r.open("/a", rw());
    let b = r.open("/b", rw());
    let e = r.extent(1);
    let w = r
        .files
        .write_at(&mut r.io, &mut r.x, a, (e, 0, 4096), 0)
        .unwrap();
    let s1 = r.files.sync(&mut r.io, b, true).unwrap();
    let s2 = r.files.sync(&mut r.io, a, true).unwrap();
    assert_eq!(r.files.usage().waiting(), 2);
    // Each completion lets the next in line go: they report in order.
    let order: Vec<_> = (0..200)
        .flat_map(|_| {
            r.turn();
            std::mem::take(&mut r.events)
        })
        .map(|ev| crate::rig::ticket_of(&ev))
        .collect();
    assert_eq!(order, vec![w, s1, s2]);
    assert_eq!(r.files.usage().waiting(), 0);
}
