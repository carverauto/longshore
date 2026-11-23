// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Adversarial Tier 5 `SimDisk` tests for `dir_log` overflow, `sync_fail`
//! truncate+reserve, `read_errors` saturation, and >64 `PlacedFault`s under `sealed`.

use super::adversarial::{mkdir_path, rename_path};
use super::{
    base_limits, dir_open, drive, make_sim, open_path, put_path, read_at, rw_create, stat_size,
    sync_dir_path, sync_slot, write_fill,
};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{BufId, Completions, FileSlot, Op, OpenFlags};
use longshore_common::sim::{DiskConfig, DiskFaults, Faults, PlacedFault, SimIo};

#[test]
fn simdisk_dir_log_overflow_poisoned_mkdir_and_sync_fail_truncate_reserve() {
    let mut out = Completions::new(16);
    dir_log_overflow_after_poisoned_dir_sync(&mut out);
    sync_fail_on_emptied_file_then_reserve_survives(&mut out);
}

/// A failed directory sync poisons `/d`; 80 renames inside it overflow the dir log.
fn dir_log_overflow_after_poisoned_dir_sync(out: &mut Completions) {
    let mut sim = make_sim(51, 512, 8, 4096);
    assert_eq!(mkdir_path(&mut sim, out, 1, "/d"), 0);
    assert_eq!(
        open_path(&mut sim, out, 2, FileSlot::new(0), "/d", dir_open()),
        0
    );
    sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
    assert_eq!(sync_slot(&mut sim, out, 3, FileSlot::new(0)), -EIO);
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(
            &mut sim,
            out,
            4,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );

    assert_eq!(
        open_path(&mut sim, out, 5, FileSlot::new(0), "/d/f", rw_create()),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            out,
            6,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );
    for i in 0..80u64 {
        let (a, b) = if i % 2 == 0 {
            ("/d/f", "/d/g")
        } else {
            ("/d/g", "/d/f")
        };
        assert_eq!(rename_path(&mut sim, out, 100 + i, a, b), 0);
    }
    assert_eq!(stat_size(&mut sim, out, 200, "/d/f"), 0);
}

/// A sync that fails after an emptying open, then a reserve, keeps the reserved size.
fn sync_fail_on_emptied_file_then_reserve_survives(out: &mut Completions) {
    let mut sim2 = make_sim(52, 512, 8, 4096);
    assert_eq!(
        open_path(&mut sim2, out, 1, FileSlot::new(0), "/tr.dat", rw_create()),
        0
    );
    assert_eq!(
        write_fill(&mut sim2, out, 2, FileSlot::new(0), 0, 512, 0x77),
        512
    );
    assert_eq!(sync_slot(&mut sim2, out, 3, FileSlot::new(0)), 0);
    assert_eq!(
        drive(
            &mut sim2,
            out,
            4,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );
    sync_dir_path(&mut sim2, out, 5, "/");

    let empty_flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_empty(true);
    assert_eq!(
        open_path(&mut sim2, out, 10, FileSlot::new(0), "/tr.dat", empty_flags),
        0
    );
    sim2.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
    assert_eq!(sync_slot(&mut sim2, out, 11, FileSlot::new(0)), -EIO);
    sim2.set_disk_faults(DiskFaults::default());

    assert_eq!(
        drive(
            &mut sim2,
            out,
            12,
            Op::FileReserve {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 1024,
                keep_size: false,
            },
        ),
        0
    );
    assert_eq!(sync_slot(&mut sim2, out, 13, FileSlot::new(0)), 0);
    sim2.power_loss();
    assert_eq!(stat_size(&mut sim2, out, 14, "/tr.dat"), 1024);
}

#[test]
fn simdisk_read_error_saturation_and_placed_faults_above_64_sealed() {
    let mut sim = two_read_error_sector_sim();
    let mut out = Completions::new(16);

    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            "/sat.dat",
            rw_create()
        ),
        0
    );
    assert_eq!(
        write_fill(&mut sim, &mut out, 2, FileSlot::new(0), 0, 2048, 0x33),
        2048
    );
    assert_eq!(sync_slot(&mut sim, &mut out, 3, FileSlot::new(0)), 0);
    sync_dir_path(&mut sim, &mut out, 4, "/");

    for i in 0..80u64 {
        sim.place_disk_fault(1000 + i, PlacedFault::Fail);
    }

    sealed(|| {
        sim.set_disk_faults(DiskFaults::NONE.with_read_error(1_000_000));
        for sec in 0..4u64 {
            assert_eq!(
                read_at(
                    &mut sim,
                    &mut out,
                    20 + sec,
                    FileSlot::new(0),
                    sec * 512,
                    512
                ),
                -EIO
            );
        }
        sim.set_disk_faults(every_fault());
        fault_storm(&mut sim, &mut out);
        let dl = put_path(&mut sim, BufId::new(0), "/sealed_dir");
        let _: i32 = drive(
            &mut sim,
            &mut out,
            500,
            Op::FileMkdir {
                path_buf: BufId::new(0),
                path_off: 0,
                path_len: dl,
            },
        );
        sim.power_loss();
    });
    assert!(sim.disk_fault_counters().read_error() >= 4);
}

/// A disk that remembers only two read-error sectors.
fn two_read_error_sector_sim() -> SimIo {
    let mut sim = SimIo::new(base_limits(), 61, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(8)
            .with_max_dirs(4)
            .with_max_bytes_per_file(32 * 1024)
            .with_max_name_len(128)
            .with_write_log_entries(16)
            .with_write_log_bytes(16 * 1024)
            .with_read_error_sectors(2)
            .with_sector_size(512),
    );
    sim
}

/// Every `DiskFaults` kind at once.
fn every_fault() -> DiskFaults {
    DiskFaults::NONE
        .with_short_write(200_000)
        .with_torn_reported(100_000)
        .with_torn_silent(100_000)
        .with_lost_reported(100_000)
        .with_lost_silent(100_000)
        .with_misdirected_write(100_000)
        .with_read_error(100_000)
        .with_bit_flip(200_000)
        .with_sync_fail(200_000)
        .with_reserve_refused(200_000)
        .with_punch_refused(200_000)
}

/// Ten rounds of write, read, sync and punch on slot 0, results ignored.
fn fault_storm(sim: &mut SimIo, out: &mut Completions) {
    for k in 0..10u64 {
        let _: i32 = write_fill(sim, out, 200 + k * 4, FileSlot::new(0), 0, 1024, 0x44);
        let _: i32 = read_at(sim, out, 201 + k * 4, FileSlot::new(0), 0, 1024);
        let _: i32 = sync_slot(sim, out, 202 + k * 4, FileSlot::new(0));
        let _: i32 = drive(
            sim,
            out,
            203 + k * 4,
            Op::FilePunch {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 512,
            },
        );
    }
}
