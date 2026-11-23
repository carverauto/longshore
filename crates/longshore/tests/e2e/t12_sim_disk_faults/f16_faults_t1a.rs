// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 16 (Part A): Zero-rate trace parity and 6 isolated write faults.

use super::helpers::{
    close_file, make_fault_sim, open_file, read_file_vec, sync_file, write_file, LINUX_EIO,
};
use longshore::io::{Completions, FileSlot, OpenFlags};
use longshore::sim::DiskFaults;

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

/// `T1-F16-2`: Each of the 6 write faults isolated at `1_000_000` ppm behaves per spec & increments counter.
#[test]
fn t1_f16_2_isolated_write_faults_and_counters() {
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let setup_base = |seed: u64| {
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
        (sim, out)
    };

    // 1. short_write
    {
        let (mut sim, mut out) = setup_base(1);
        sim.set_disk_faults(DiskFaults {
            short_write: 1_000_000,
            ..Default::default()
        });
        let r = write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x22; 2048]);
        assert!(r > 0 && r < 2048 && r % 512 == 0, "short_write={r}");
        sim.set_disk_faults(DiskFaults::default());
        let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
        assert!(got[..usize::try_from(r).unwrap()]
            .iter()
            .all(|&b| b == 0x22));
        assert!(got[usize::try_from(r).unwrap()..]
            .iter()
            .all(|&b| b == 0x11));
        assert_eq!(sim.disk_fault_counters().short_write, 1);
    }

    // 2. torn_reported
    {
        let (mut sim, mut out) = setup_base(2);
        sim.set_disk_faults(DiskFaults {
            torn_reported: 1_000_000,
            ..Default::default()
        });
        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x33; 2048]),
            -LINUX_EIO
        );
        sim.set_disk_faults(DiskFaults::default());
        let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
        let prefix = got.iter().take_while(|&&b| b == 0x33).count();
        assert!(prefix > 0 && prefix < 2048 && prefix % 512 == 0);
        assert_eq!(sim.disk_fault_counters().torn_reported, 1);
    }

    // 3. torn_silent
    {
        let (mut sim, mut out) = setup_base(3);
        sim.set_disk_faults(DiskFaults {
            torn_silent: 1_000_000,
            ..Default::default()
        });
        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x44; 2048]),
            2048
        );
        sim.set_disk_faults(DiskFaults::default());
        let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
        let prefix = got.iter().take_while(|&&b| b == 0x44).count();
        assert!(prefix > 0 && prefix < 2048 && prefix % 512 == 0);
        assert_eq!(sim.disk_fault_counters().torn_silent, 1);
    }

    // 4. lost_reported
    {
        let (mut sim, mut out) = setup_base(4);
        sim.set_disk_faults(DiskFaults {
            lost_reported: 1_000_000,
            ..Default::default()
        });
        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x55; 2048]),
            -LINUX_EIO
        );
        sim.set_disk_faults(DiskFaults::default());
        let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
        assert!(got.iter().all(|&b| b == 0x11));
        assert_eq!(sim.disk_fault_counters().lost_reported, 1);
    }

    // 5. lost_silent
    {
        let (mut sim, mut out) = setup_base(5);
        sim.set_disk_faults(DiskFaults {
            lost_silent: 1_000_000,
            ..Default::default()
        });
        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x66; 2048]),
            2048
        );
        sim.set_disk_faults(DiskFaults::default());
        let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
        assert!(got.iter().all(|&b| b == 0x11));
        assert_eq!(sim.disk_fault_counters().lost_silent, 1);
    }

    // 6. misdirected_write
    {
        let (mut sim, mut out) = setup_base(6);
        sim.set_disk_faults(DiskFaults {
            misdirected_write: 1_000_000,
            ..Default::default()
        });
        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x77; 512]),
            512
        );
        sim.set_disk_faults(DiskFaults::default());
        let got = read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 2048).unwrap();
        assert!(got[..512].iter().all(|&b| b == 0x11));
        assert!(got[512..].chunks(512).any(|s| s.iter().all(|&b| b == 0x77)));
        assert_eq!(sim.disk_fault_counters().misdirected_write, 1);
    }
}
