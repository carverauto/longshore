// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Disk `sync_fail` (`AS_EIO`), placed-fault slot consistency, `bit_flip`, and
//! refused reserve/punch tests.

use super::{base_limits, drive, make_sim, open_path, rw_create, rw_open, sync_dir_path};
use longshore_common::io::posix::errno::linux::{EIO, ENOSPC, EOPNOTSUPP};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskConfig, DiskFaults, Faults, PlacedFault, SimIo};

#[test]
fn sync_fail_as_eio_poisoned_writes_never_become_durable_unless_overwritten() {
    let mut sim = make_sim(23, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, FileSlot(0), "/eio.dat", rw_create()),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sim.buf(BufId(1))[..1536].fill(0x11);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            20,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 1536,
                file_off: 0,
            },
        ),
        1536
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            21,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );

    sim.buf(BufId(1))[..1536].fill(0x22);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            22,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 1536,
                file_off: 0,
            },
        ),
        1536
    );
    sim.set_disk_faults(DiskFaults {
        sync_fail: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            23,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        -EIO
    );
    sim.set_disk_faults(DiskFaults::default());

    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            24,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 1536,
                file_off: 0,
            },
        ),
        1536
    );
    assert!(sim.buf(BufId(2))[..1536].iter().all(|&b| b == 0x22));

    sim.buf(BufId(1))[..512].fill(0x33);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            25,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 512,
                file_off: 512,
            },
        ),
        512
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            26,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );

    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            27,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 1536,
                file_off: 0,
            },
        ),
        1536
    );
    let live = sim.buf(BufId(2));
    assert!(live[..512].iter().all(|&b| b == 0x22));
    assert!(live[512..1024].iter().all(|&b| b == 0x33));
    assert!(live[1024..1536].iter().all(|&b| b == 0x22));

    sim.power_loss();
    assert_eq!(
        open_path(&mut sim, &mut out, 30, FileSlot(0), "/eio.dat", rw_open()),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            31,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 1536,
                file_off: 0,
            },
        ),
        1536
    );
    let dur = sim.buf(BufId(2));
    assert!(dur[..512].iter().all(|&b| b == 0x11));
    assert!(dur[512..1024].iter().all(|&b| b == 0x33));
    assert!(dur[1024..1536].iter().all(|&b| b == 0x11));
}

#[test]
fn placed_faults_slot_consistency_bit_flip_and_refused_reserve_punch() {
    let mut sim = SimIo::new(
        base_limits(),
        31,
        Faults {
            delay: 500_000,
            ..Default::default()
        },
    );
    sim.setup_disk(DiskConfig {
        max_files: 8,
        max_dirs: 4,
        max_bytes_per_file: 32 * 1024,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: 16 * 1024,
        read_error_sectors: 8,
        sector_size: 512,
    });
    let mut out = Completions::new(16);

    sim.place_disk_fault(1, PlacedFault::FailAfter);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, FileSlot(0), "/p.dat", rw_create()),
        -EIO
    );
    assert_eq!(
        open_path(&mut sim, &mut out, 2, FileSlot(0), "/p.dat", rw_open()),
        0
    );
    sim.place_disk_fault(3, PlacedFault::Fail);
    assert_eq!(
        drive(&mut sim, &mut out, 3, Op::FileClose { slot: FileSlot(0) }),
        -EIO
    );
    assert_eq!(
        open_path(&mut sim, &mut out, 4, FileSlot(0), "/p.dat", rw_open()),
        0
    );

    sim.set_disk_faults(DiskFaults {
        reserve_refused: 1_000_000,
        punch_refused: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            5,
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
        ),
        -ENOSPC
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            6,
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
            },
        ),
        -EOPNOTSUPP
    );

    sim.set_disk_faults(DiskFaults::default());
    sim.buf(BufId(1))[..64].fill(0xAA);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            7,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 64,
                file_off: 0,
            },
        ),
        64
    );
    sim.set_disk_faults(DiskFaults {
        bit_flip: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            8,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 64,
                file_off: 0,
            },
        ),
        64
    );
    let diffs: usize = sim.buf(BufId(2))[..64]
        .iter()
        .map(|&b| (b ^ 0xAA).count_ones() as usize)
        .sum();
    assert_eq!(diffs, 1);

    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            9,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 64,
                file_off: 0,
            },
        ),
        64
    );
    assert!(sim.buf(BufId(2))[..64].iter().all(|&b| b == 0xAA));

    sim.place_disk_fault(14, PlacedFault::Fail);
    sync_dir_path(&mut sim, &mut out, 12, "/");
    sim.power_loss();
    assert_eq!(
        open_path(&mut sim, &mut out, 10, FileSlot(0), "/p.dat", rw_open()),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            11,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 64,
                file_off: 0,
            },
        ),
        -EIO
    );
    let c = sim.disk_fault_counters();
    assert_eq!(c.placed_fail, 2);
    assert_eq!(c.placed_fail_after, 1);
    assert_eq!(c.reserve_refused, 1);
    assert_eq!(c.punch_refused, 1);
    assert_eq!(c.bit_flip, 1);
}
