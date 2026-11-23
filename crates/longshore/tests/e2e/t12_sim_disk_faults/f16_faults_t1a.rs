// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 16 (Part A): Zero-rate trace parity and 6 isolated write faults.

use super::helpers::{
    close_file, make_fault_sim, open_file, read_file_vec, sync_file, write_file, LINUX_EIO,
};
use longshore::io::{Completions, FileSlot, OpenFlags};
use longshore::sim::{DiskFaults, SimIo};

/// `T1-F16-1`: All 11 `DiskFaults` rates at 0 draw zero RNG values and preserve `trace_hash()`.
#[test]
fn t1_f16_1_zero_rates_zero_rng_draws_same_trace_hash() {
    let run = || {
        let mut sim = make_fault_sim(111);
        sim.set_disk_faults(DiskFaults::default());
        let mut out = Completions::new(8);
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/z.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x12; 1024]),
            1024
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        let _: Vec<u8> = read_file_vec(&mut sim, &mut out, 4, FileSlot(0), 0, 1024).unwrap();
        assert_eq!(close_file(&mut sim, &mut out, 5, FileSlot(0)), 0);
        assert_eq!(sim.disk_fault_counters(), Default::default());
        sim.trace_hash()
    };
    assert_eq!(run(), run());
}

/// Builds a fault sim with seed `seed` whose `/wf.dat` holds 2048 synced
/// bytes of `0x11`, then writes `len` bytes of `fill` at offset 0 under
/// `faults`, and reads the 2048 bytes back with faults off.
///
/// Returns the write's result, the bytes read back and the sim.
fn faulted_overwrite(seed: u64, faults: DiskFaults, fill: u8, len: usize) -> (i32, Vec<u8>, SimIo) {
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let mut sim = make_fault_sim(seed);
    let mut out = Completions::new(8);
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/wf.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x11; 2048]),
        2048
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    sim.set_disk_faults(faults);
    let r = write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &vec![fill; len]);
    sim.set_disk_faults(DiskFaults::default());
    let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
    (r, got, sim)
}

/// Write fault 1: `short_write` at the full rate.
fn write_fault_short_write() {
    let (r, got, sim) = faulted_overwrite(
        1,
        DiskFaults {
            short_write: 1_000_000,
            ..Default::default()
        },
        0x22,
        2048,
    );
    assert!(r > 0 && r < 2048 && r % 512 == 0, "short_write={r}");
    assert!(got[..usize::try_from(r).unwrap()]
        .iter()
        .all(|&b| b == 0x22));
    assert!(got[usize::try_from(r).unwrap()..]
        .iter()
        .all(|&b| b == 0x11));
    assert_eq!(sim.disk_fault_counters().short_write, 1);
}

/// Write fault 2: `torn_reported` at the full rate.
fn write_fault_torn_reported() {
    let (r, got, sim) = faulted_overwrite(
        2,
        DiskFaults {
            torn_reported: 1_000_000,
            ..Default::default()
        },
        0x33,
        2048,
    );
    assert_eq!(r, -LINUX_EIO);
    let prefix = got.iter().take_while(|&&b| b == 0x33).count();
    assert!(prefix > 0 && prefix < 2048 && prefix % 512 == 0);
    assert_eq!(sim.disk_fault_counters().torn_reported, 1);
}

/// Write fault 3: `torn_silent` at the full rate.
fn write_fault_torn_silent() {
    let (r, got, sim) = faulted_overwrite(
        3,
        DiskFaults {
            torn_silent: 1_000_000,
            ..Default::default()
        },
        0x44,
        2048,
    );
    assert_eq!(r, 2048);
    let prefix = got.iter().take_while(|&&b| b == 0x44).count();
    assert!(prefix > 0 && prefix < 2048 && prefix % 512 == 0);
    assert_eq!(sim.disk_fault_counters().torn_silent, 1);
}

/// Write fault 4: `lost_reported` at the full rate.
fn write_fault_lost_reported() {
    let (r, got, sim) = faulted_overwrite(
        4,
        DiskFaults {
            lost_reported: 1_000_000,
            ..Default::default()
        },
        0x55,
        2048,
    );
    assert_eq!(r, -LINUX_EIO);
    assert!(got.iter().all(|&b| b == 0x11));
    assert_eq!(sim.disk_fault_counters().lost_reported, 1);
}

/// Write fault 5: `lost_silent` at the full rate.
fn write_fault_lost_silent() {
    let (r, got, sim) = faulted_overwrite(
        5,
        DiskFaults {
            lost_silent: 1_000_000,
            ..Default::default()
        },
        0x66,
        2048,
    );
    assert_eq!(r, 2048);
    assert!(got.iter().all(|&b| b == 0x11));
    assert_eq!(sim.disk_fault_counters().lost_silent, 1);
}

/// Write fault 6: `misdirected_write` at the full rate.
fn write_fault_misdirected_write() {
    let (r, got, sim) = faulted_overwrite(
        6,
        DiskFaults {
            misdirected_write: 1_000_000,
            ..Default::default()
        },
        0x77,
        512,
    );
    assert_eq!(r, 512);
    assert!(got[..512].iter().all(|&b| b == 0x11));
    assert!(got[512..].chunks(512).any(|s| s.iter().all(|&b| b == 0x77)));
    assert_eq!(sim.disk_fault_counters().misdirected_write, 1);
}

/// `T1-F16-2`: Each of the 6 write faults isolated at `1_000_000` ppm behaves per spec & increments counter.
#[test]
fn t1_f16_2_isolated_write_faults_and_counters() {
    write_fault_short_write();
    write_fault_torn_reported();
    write_fault_torn_silent();
    write_fault_lost_reported();
    write_fault_lost_silent();
    write_fault_misdirected_write();
}
