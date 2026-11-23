// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` placed-fault (`Fail`, `FailAfter`) tests.

use super::{drive, make_storage, open_file, read_all, rw_create, sync_dir, write_bytes};
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{FileSlot, Op};
use longshore_common::sim::PlacedFault;

#[test]
fn placed_fail_and_fail_after_and_sync_unstable() {
    let (mut sim, mut out, mut tok) = make_storage(1, 512);
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            tok,
            FileSlot::new(0),
            "/d/f",
            rw_create()
        ),
        0
    );
    tok += 1;
    sync_dir(&mut sim, &mut out, tok, "/d");
    tok += 5;

    sim.place_disk_fault(9, PlacedFault::Fail);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"data"),
        -EIO
    );
    tok += 1;
    assert_eq!(read_all(&mut sim, &mut out, tok, "/d/f"), Ok(Vec::new()));
    tok += 30;
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"data"),
        4
    );
    tok += 1;

    sim.place_disk_fault(14, PlacedFault::FailAfter);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"next"),
        -EIO
    );
    tok += 1;
    sim.place_disk_fault(15, PlacedFault::FailAfter);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: false,
            },
        ),
        -EIO
    );
    tok += 1;
    sim.power_loss();
    assert_eq!(
        read_all(&mut sim, &mut out, tok, "/d/f"),
        Ok(b"next".to_vec())
    );
}

#[test]
fn placed_fail_on_sync_leaves_contents_unsynced() {
    let mut saw_absent = false;
    for seed in 0..16u64 {
        let (mut sim, mut out, mut tok) = make_storage(seed, 512);
        assert_eq!(
            open_file(
                &mut sim,
                &mut out,
                tok,
                FileSlot::new(0),
                "/d/f",
                rw_create()
            ),
            0
        );
        tok += 1;
        sync_dir(&mut sim, &mut out, tok, "/d");
        tok += 5;
        assert_eq!(
            write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"data"),
            4
        );
        tok += 1;
        sim.place_disk_fault(10, PlacedFault::Fail);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                tok,
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: false,
                },
            ),
            -EIO
        );
        tok += 1;
        sim.power_loss();
        if read_all(&mut sim, &mut out, tok, "/d/f")
            .unwrap()
            .is_empty()
        {
            saw_absent = true;
        }
    }
    assert!(saw_absent);
}
