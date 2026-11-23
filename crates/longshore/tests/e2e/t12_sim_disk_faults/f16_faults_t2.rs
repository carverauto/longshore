// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 16: `AS_EIO` sync, persistent read error clearing, and placement rules.

use super::super::harness::submit_and_wait_one;
use super::helpers::{
    close_file, make_fault_sim, open_file, read_file_vec, sync_file, sync_root_dir, write_file,
    LINUX_EIO,
};
use longshore::io::{Completions, FileSlot, Op, OpenFlags, Token};
use longshore::sim::{DiskFaults, PlacedFault};

/// `T2-F16-1`: `AS_EIO` semantics: failed `FileSync` writes remain readable, never durable on retry.
#[test]
fn t2_f16_1_failed_sync_as_eio_poisoned_writes_vanish_on_crash() {
    let mut sim = make_fault_sim(60);
    let mut out = Completions::new(8);
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot::new(0), "/aseio.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot::new(0), 0, &[0x11; 512]),
        512
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot::new(0), false), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot::new(1)), 0);

    // Write 0x22 and fail its FileSync (-EIO).
    assert_eq!(
        write_file(&mut sim, &mut out, 10, FileSlot::new(0), 0, &[0x22; 512]),
        512
    );
    sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
    assert_eq!(
        sync_file(&mut sim, &mut out, 11, FileSlot::new(0), false),
        -LINUX_EIO
    );
    sim.set_disk_faults(DiskFaults::default());

    // Pre-crash read still sees 0x22.
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 12, FileSlot::new(0), 0, 512).unwrap(),
        vec![0x22; 512]
    );
    // Second FileSync succeeds, but must NOT salvage poisoned pages from failed sync.
    assert_eq!(
        sync_file(&mut sim, &mut out, 13, FileSlot::new(0), false),
        0
    );
    assert_eq!(close_file(&mut sim, &mut out, 14, FileSlot::new(0)), 0);

    sim.power_loss();

    assert_eq!(
        open_file(&mut sim, &mut out, 20, FileSlot::new(0), "/aseio.dat", rw),
        0
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 21, FileSlot::new(0), 0, 512).unwrap(),
        vec![0x11; 512],
        "poisoned write from failed sync must be absent after power_loss"
    );
}

/// `T2-F16-2`: Sector `read_error` ignores partial write; cleared only by full-sector write or punch.
#[test]
fn t2_f16_2_read_error_cleared_only_by_full_sector_write_or_punch() {
    let mut sim = make_fault_sim(61);
    let mut out = Completions::new(8);
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot::new(0), "/rclr.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot::new(0), 0, &[0xAA; 8192]),
        8192
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot::new(0), false), 0);

    // Mark sector 0 ([0..512)) and sector 8 ([4096..4608)) with read errors.
    sim.set_disk_faults(DiskFaults::NONE.with_read_error(1_000_000));
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 4, FileSlot::new(0), 0, 512),
        Err(-LINUX_EIO)
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 5, FileSlot::new(0), 4096, 512),
        Err(-LINUX_EIO)
    );
    sim.set_disk_faults(DiskFaults::default());

    // Partial write of [0..256) does NOT clear sector 0's read_error.
    assert_eq!(
        write_file(&mut sim, &mut out, 6, FileSlot::new(0), 0, &[0xBB; 256]),
        256
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 7, FileSlot::new(0), 0, 512),
        Err(-LINUX_EIO)
    );

    // Full-sector write [0..512) clears sector 0's read_error.
    assert_eq!(
        write_file(&mut sim, &mut out, 8, FileSlot::new(0), 0, &[0xCC; 512]),
        512
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 9, FileSlot::new(0), 0, 512).unwrap(),
        vec![0xCC; 512]
    );

    // Punching [4096..8192) (block-aligned) clears sector 8's read_error and returns zeros.
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token::new(10),
            Op::FilePunch {
                slot: FileSlot::new(0),
                file_off: 4096,
                len: 4096,
            },
            10_000,
        )
        .unwrap()
        .result(),
        0
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 11, FileSlot::new(0), 4096, 512).unwrap(),
        vec![0u8; 512]
    );
}

/// `T2-F16-3`: `read_bit_flip` alters only the returned buffer, leaving the medium untouched.
#[test]
fn t2_f16_3_read_bit_flip_leaves_medium_untouched() {
    let mut sim = make_fault_sim(62);
    let mut out = Completions::new(8);
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot::new(0), "/flip.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot::new(0), 0, &[0x55; 512]),
        512
    );

    sim.set_disk_faults(DiskFaults::NONE.with_bit_flip(1_000_000));
    let bad = read_file_vec(&mut sim, &mut out, 3, FileSlot::new(0), 0, 512).unwrap();
    assert_ne!(bad, vec![0x55; 512]);

    sim.set_disk_faults(DiskFaults::default());
    let clean = read_file_vec(&mut sim, &mut out, 4, FileSlot::new(0), 0, 512).unwrap();
    assert_eq!(clean, vec![0x55; 512]);
}

/// `T2-F16-4`: `misdirected_write` on a 1-sector file is not misdirected out of bounds.
#[test]
fn t2_f16_4_misdirected_write_on_single_sector_file_not_misdirected() {
    let mut sim = make_fault_sim(63);
    let mut out = Completions::new(8);
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot::new(0), "/1sec.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot::new(0), 0, &[0x11; 512]),
        512
    );

    sim.set_disk_faults(DiskFaults::NONE.with_misdirected_write(1_000_000));
    assert_eq!(
        write_file(&mut sim, &mut out, 3, FileSlot::new(0), 0, &[0x22; 512]),
        512
    );
    sim.set_disk_faults(DiskFaults::default());

    let got = read_file_vec(&mut sim, &mut out, 4, FileSlot::new(0), 0, 512).unwrap();
    assert_eq!(got, vec![0x22; 512]);
}

/// `T2-F16-5`: Placing a write fault on a non-write op (`FileSync`) acts as `PlacedFault::Fail`.
#[test]
fn t2_f16_5_write_fault_placed_on_sync_acts_as_fail() {
    let mut sim = make_fault_sim(64);
    let mut out = Completions::new(8);
    // Op 1: Open, Op 2: Write, Op 3: FileSync -> place TornReported on Op 3.
    sim.place_disk_fault(3, PlacedFault::TornReported);

    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot::new(0), "/plsync.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot::new(0), 0, &[0x11; 512]),
        512
    );
    assert_eq!(
        sync_file(&mut sim, &mut out, 3, FileSlot::new(0), false),
        -LINUX_EIO
    );
    assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot::new(0)), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 5, FileSlot::new(1)), 0);

    sim.power_loss();

    assert_eq!(
        open_file(&mut sim, &mut out, 10, FileSlot::new(0), "/plsync.dat", rw),
        0
    );
    let got = read_file_vec(&mut sim, &mut out, 11, FileSlot::new(0), 0, 512).unwrap();
    assert!(
        got.is_empty(),
        "failed sync must leave synced file empty on crash"
    );
}
