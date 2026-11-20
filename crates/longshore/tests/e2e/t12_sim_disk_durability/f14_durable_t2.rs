// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 14: Writeback eviction, capacity limits, trace parity, and overlays.

use super::super::harness::{submit_and_wait_one, write_nul_path};
use super::helpers::{
    close_file, disk_limits, make_disk_sim, open_file, read_file_vec, stat_exists, sync_file,
    sync_root_dir, write_file, LINUX_EINVAL, LINUX_EMFILE, LINUX_ENAMETOOLONG, LINUX_ENOSPC,
};
use longshore::io::{BufId, Completions, FileSlot, Op, OpenFlags, Token};
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};

/// `T2-F14-1`: Overflowing `write_log` entries evicts oldest writes to durable without refusing writes.
#[test]
fn t2_f14_1_write_log_overflow_evicts_oldest_to_durable() {
    let mut sim = make_disk_sim(10, 512, 4, 2048);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/evict.dat", rw),
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 2, FileSlot(1)), 0);

    for i in 0..8u64 {
        let data = vec![(0x10 + i) as u8; 512];
        assert_eq!(
            write_file(&mut sim, &mut out, 10 + i, FileSlot(0), i * 512, &data),
            512
        );
    }
    assert_eq!(close_file(&mut sim, &mut out, 30, FileSlot(0)), 0);

    sim.power_loss();

    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 40, FileSlot(0), "/evict.dat", ro),
        0
    );
    // Oldest 4 entries (sectors 0..4) were evicted to durable when entries 4..8 entered the 4-entry log.
    for i in 0..4u64 {
        let got =
            read_file_vec(&mut sim, &mut out, 50 + i, FileSlot(0), i * 512, 512).expect("read");
        assert!(
            got.iter().all(|&b| b == (0x10 + i) as u8),
            "evicted sector {i} must survive power_loss"
        );
    }
    assert_eq!(close_file(&mut sim, &mut out, 60, FileSlot(0)), 0);
}

/// `T2-F14-2`: Rename without dir sync + `power_loss()` yields old or new name, never both or neither.
#[test]
fn t2_f14_2_unsynced_rename_crash_yields_old_or_new_never_both() {
    let (mut saw_old, mut saw_new) = (false, false);
    for seed in 0..32u64 {
        let mut sim = make_disk_sim(seed, 512, 32, 64 * 1024);
        let mut out = Completions::new(8);
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/old.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, b"payload"),
            7
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot(0)), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 5, FileSlot(1)), 0);

        let l_old = write_nul_path(&mut sim, BufId(0), 0, "/old.dat");
        let l_new = write_nul_path(&mut sim, BufId(1), 0, "/new.dat");
        assert_eq!(
            submit_and_wait_one(
                &mut sim,
                &mut out,
                Token(10),
                Op::FileRename {
                    old_buf: BufId(0),
                    old_off: 0,
                    old_len: l_old,
                    new_buf: BufId(1),
                    new_off: 0,
                    new_len: l_new,
                },
                10_000,
            )
            .expect("rename")
            .result,
            0
        );
        sim.power_loss();

        let has_old = stat_exists(&mut sim, &mut out, 20, "/old.dat");
        let has_new = stat_exists(&mut sim, &mut out, 21, "/new.dat");
        assert_ne!(
            has_old, has_new,
            "seed {seed}: expected exclusive old or new, got ({has_old}, {has_new})"
        );
        saw_old |= has_old;
        saw_new |= has_new;
    }
    assert!(
        saw_old && saw_new,
        "expected both crash outcomes across seeds"
    );
}

/// `T2-F14-3`: Exceeding configured `max_files`, `max_bytes_per_file`, or `max_name_len` fails cleanly.
#[test]
fn t2_f14_3_exceed_disk_capacities_deterministic_error() {
    let mut sim = SimIo::new(disk_limits(), 99, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 2,
        max_dirs: 2,
        max_bytes_per_file: 4096,
        max_name_len: 16,
        write_log_entries: 16,
        write_log_bytes: 16_384,
        read_error_sectors: 4,
        sector_size: 512,
    });
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };

    // 1. Name longer than max_name_len (16).
    let r_long = open_file(
        &mut sim,
        &mut out,
        1,
        FileSlot(0),
        "/too_long_filename.dat",
        rw,
    );
    assert!(
        r_long == -LINUX_ENAMETOOLONG || r_long == -LINUX_EINVAL,
        "unexpected long-name code: {r_long}"
    );

    // 2. Exceed max_bytes_per_file (4096).
    assert_eq!(
        open_file(&mut sim, &mut out, 2, FileSlot(0), "/f1.dat", rw),
        0
    );
    let r_big = write_file(&mut sim, &mut out, 3, FileSlot(0), 4096, b"past-max");
    assert_eq!(r_big, -LINUX_ENOSPC);
    assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot(0)), 0);

    // 3. Exceed max_files (2).
    assert_eq!(
        open_file(&mut sim, &mut out, 5, FileSlot(0), "/f2.dat", rw),
        0
    );
    assert_eq!(close_file(&mut sim, &mut out, 6, FileSlot(0)), 0);
    let r_many = open_file(&mut sim, &mut out, 7, FileSlot(0), "/f3.dat", rw);
    assert!(
        r_many == -LINUX_ENOSPC || r_many == -LINUX_EMFILE,
        "expected capacity error on 3rd file, got {r_many}"
    );
}

/// `T2-F14-4`: Zero `DiskFaults` preserves exact `trace_hash()` and leaves all fault counts at 0.
#[test]
fn t2_f14_4_zero_disk_faults_preserves_trace_hash() {
    let run = || {
        let mut sim = make_disk_sim(77, 512, 32, 64 * 1024);
        sim.set_disk_faults(DiskFaults::default());
        let mut out = Completions::new(8);
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/tr.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, b"trace-data"),
            10
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), true), 0);
        assert_eq!(
            read_file_vec(&mut sim, &mut out, 4, FileSlot(0), 0, 10).unwrap(),
            b"trace-data"
        );
        assert_eq!(close_file(&mut sim, &mut out, 5, FileSlot(0)), 0);
        (sim.trace_hash(), sim.disk_fault_counters())
    };
    let (h1, c1) = run();
    let (h2, c2) = run();
    assert_eq!(h1, h2);
    assert_eq!(c1, Default::default());
    assert_eq!(c2, Default::default());
}

/// `T2-F14-5`: Overlapping unsynced writes overlay in exact submission order.
#[test]
fn t2_f14_5_overlapping_unsynced_writes_overlay_exact_bytes() {
    let mut sim = make_disk_sim(12, 512, 32, 64 * 1024);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/lap.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0xAA; 1024]),
        1024
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 3, FileSlot(0), 512, &[0xBB; 1024]),
        1024
    );

    let got = read_file_vec(&mut sim, &mut out, 4, FileSlot(0), 0, 1536).expect("read");
    assert_eq!(got.len(), 1536);
    assert!(got[..512].iter().all(|&b| b == 0xAA));
    assert!(got[512..1536].iter().all(|&b| b == 0xBB));
    assert_eq!(close_file(&mut sim, &mut out, 5, FileSlot(0)), 0);
}
