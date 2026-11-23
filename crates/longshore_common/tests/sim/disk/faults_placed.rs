// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Placed-fault slot consistency, `bit_flip`, and refused reserve/punch tests.

use super::{
    base_limits, drive, open_path, read_at, rw_create, rw_open, sync_dir_path, write_fill,
};
use longshore_common::io::posix::errno::linux::{EIO, ENOSPC, EOPNOTSUPP};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskConfig, DiskFaults, Faults, PlacedFault, SimIo};

#[test]
fn placed_faults_slot_consistency_bit_flip_and_refused_reserve_punch() {
    let mut sim = delayed_sim();
    let mut out = Completions::new(16);

    sim.place_disk_fault(1, PlacedFault::FailAfter);
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            "/p.dat",
            rw_create()
        ),
        -EIO
    );
    assert_eq!(
        open_path(&mut sim, &mut out, 2, FileSlot::new(0), "/p.dat", rw_open()),
        0
    );
    sim.place_disk_fault(3, PlacedFault::Fail);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            3,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        -EIO
    );
    assert_eq!(
        open_path(&mut sim, &mut out, 4, FileSlot::new(0), "/p.dat", rw_open()),
        0
    );

    reserve_and_punch_are_refused(&mut sim, &mut out);
    bit_flip_corrupts_one_read_only(&mut sim, &mut out);

    sim.place_disk_fault(14, PlacedFault::Fail);
    sync_dir_path(&mut sim, &mut out, 12, "/");
    sim.power_loss();
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            10,
            FileSlot::new(0),
            "/p.dat",
            rw_open()
        ),
        0
    );
    assert_eq!(
        read_at(&mut sim, &mut out, 11, FileSlot::new(0), 0, 64),
        -EIO
    );
    let c = sim.disk_fault_counters();
    assert_eq!(c.placed_fail(), 2);
    assert_eq!(c.placed_fail_after(), 1);
    assert_eq!(c.reserve_refused(), 1);
    assert_eq!(c.punch_refused(), 1);
    assert_eq!(c.bit_flip(), 1);
}

/// A disk whose operations are delayed half the time.
fn delayed_sim() -> SimIo {
    let mut sim = SimIo::new(base_limits(), 31, Faults::NONE.with_delay(500_000));
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(8)
            .with_max_dirs(4)
            .with_max_bytes_per_file(32 * 1024)
            .with_max_name_len(128)
            .with_write_log_entries(16)
            .with_write_log_bytes(16 * 1024)
            .with_read_error_sectors(8)
            .with_sector_size(512),
    );
    sim
}

/// With both refusal rates maxed, reserve gives `-ENOSPC` and punch `-EOPNOTSUPP` (tokens 5, 6).
fn reserve_and_punch_are_refused(sim: &mut SimIo, out: &mut Completions) {
    sim.set_disk_faults(
        DiskFaults::NONE
            .with_reserve_refused(1_000_000)
            .with_punch_refused(1_000_000),
    );
    assert_eq!(
        drive(
            sim,
            out,
            5,
            Op::FileReserve {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
        ),
        -ENOSPC
    );
    assert_eq!(
        drive(
            sim,
            out,
            6,
            Op::FilePunch {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 4096,
            },
        ),
        -EOPNOTSUPP
    );
}

/// A `bit_flip` read differs by exactly one bit; the next clean read is intact (tokens 7..10).
fn bit_flip_corrupts_one_read_only(sim: &mut SimIo, out: &mut Completions) {
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(write_fill(sim, out, 7, FileSlot::new(0), 0, 64, 0xAA), 64);
    sim.set_disk_faults(DiskFaults::NONE.with_bit_flip(1_000_000));
    assert_eq!(read_at(sim, out, 8, FileSlot::new(0), 0, 64), 64);
    let diffs: usize = sim.buf(BufId::new(2))[..64]
        .iter()
        .map(|&b| (b ^ 0xAA).count_ones() as usize)
        .sum();
    assert_eq!(diffs, 1);

    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(read_at(sim, out, 9, FileSlot::new(0), 0, 64), 64);
    assert!(sim.buf(BufId::new(2))[..64].iter().all(|&b| b == 0xAA));
}
