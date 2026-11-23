// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 17: WAL edge cases, `NOTICE` attribution, and license headers.

use super::super::harness::WalEngine;
use super::helpers::{
    close_file, make_fault_sim, open_file, repo_root, sync_file, sync_root_dir, wal_config,
    write_file, LINUX_EIO,
};
use longshore::io::{Completions, FileSlot, OpenFlags};
use longshore::sim::DiskFaults;

/// `T2-F17-1`: Failed `FileSync` mid-batch excludes uncommitted WAL records after `power_loss()`.
#[test]
fn t2_f17_1_wal_failed_sync_mid_batch_excludes_uncommitted_after_crash() {
    let mut sim = make_fault_sim(181);
    let mut out = Completions::new(16);
    let mut wal = WalEngine::new(wal_config());
    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot::new(2)), 0);

    for i in 1..=2u64 {
        assert_eq!(
            wal.append_chained(&mut sim, &mut out, &[i as u8; 32])
                .unwrap(),
            (4096, 0)
        );
    }
    sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
    let (w3, s3) = wal.append_chained(&mut sim, &mut out, &[3; 32]).unwrap();
    assert_eq!((w3, s3), (4096, -LINUX_EIO));
    sim.set_disk_faults(DiskFaults::default());

    sim.power_loss();
    wal.reset_after_power_loss();
    let stats = wal.recover(&mut sim, &mut out, 4, &mut |_| {}).unwrap();
    assert_eq!(stats.valid_records, 2);
    assert_eq!(stats.last_seq, 2);
}

/// `T2-F17-2`: WAL segment `FileRename` rotation (with and without dir sync) recovers via fixed names.
#[test]
fn t2_f17_2_wal_segment_rename_rotation_with_and_without_dir_sync() {
    // 1. With directory sync: wal_0001.log -> wal_0002.log survives crash.
    {
        let mut sim = make_fault_sim(182);
        let mut out = Completions::new(16);
        let mut wal = WalEngine::new(wal_config());
        assert_eq!(
            wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
            0
        );
        assert_eq!(
            wal.append_chained(&mut sim, &mut out, b"rotated-rec")
                .unwrap(),
            (4096, 0)
        );
        assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);
        assert_eq!(
            wal.rename_segment(&mut sim, &mut out, 1, 2, true).unwrap(),
            0
        );

        sim.power_loss();
        wal.reset_after_power_loss();
        let mut segs = Vec::new();
        let stats = wal
            .recover(&mut sim, &mut out, 4, &mut |r| {
                segs.push((r.seg_idx, r.seq))
            })
            .unwrap();
        assert_eq!(stats.valid_records, 1);
        assert_eq!(segs, vec![(2, 1)]);
    }

    // 2. Without directory sync: either seg 1 or seg 2 exists after crash, never lost or doubled.
    for seed in 0..20u64 {
        let mut sim = make_fault_sim(seed);
        let mut out = Completions::new(16);
        let mut wal = WalEngine::new(wal_config());
        assert_eq!(
            wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
            0
        );
        assert_eq!(
            wal.append_chained(&mut sim, &mut out, b"unsynced-rot")
                .unwrap(),
            (4096, 0)
        );
        assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot::new(2)), 0);
        assert_eq!(
            wal.rename_segment(&mut sim, &mut out, 1, 2, false).unwrap(),
            0
        );

        sim.power_loss();
        wal.reset_after_power_loss();
        let stats = wal.recover(&mut sim, &mut out, 4, &mut |_| {}).unwrap();
        assert_eq!(stats.segments_found, 1);
        assert_eq!(stats.valid_records, 1);
    }
}

/// `T2-F17-3`: Sector `read_error` during WAL scan sets `stopped_on_read_error` cleanly without panic.
#[test]
fn t2_f17_3_wal_sector_read_error_stops_cleanly() {
    let mut sim = make_fault_sim(183);
    let mut out = Completions::new(16);
    let mut wal = WalEngine::new(wal_config());
    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot::new(2)), 0);
    for i in 1..=3u64 {
        assert_eq!(
            wal.append_chained(&mut sim, &mut out, &[i as u8; 40])
                .unwrap(),
            (4096, 0)
        );
    }
    assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);

    // Mark record #2's sector (offset 4096) with a persistent read error.
    let ro = OpenFlags::NONE.with_read(true);
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            50,
            FileSlot::new(0),
            "/wal_0001.log",
            ro
        ),
        0
    );
    sim.set_disk_faults(DiskFaults::NONE.with_read_error(1_000_000));
    assert!(
        super::helpers::read_file_vec(&mut sim, &mut out, 51, FileSlot::new(0), 4096, 512).is_err(),
        "the injected read error must fire"
    );
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(close_file(&mut sim, &mut out, 52, FileSlot::new(0)), 0);

    let stats = wal.recover(&mut sim, &mut out, 2, &mut |_| {}).unwrap();
    assert_eq!(stats.valid_records, 1);
    assert!(stats.stopped_on_read_error);
}

/// `T2-F17-4`: Empty zero-length WAL segment and mid-header torn write are handled cleanly.
#[test]
fn t2_f17_4_wal_empty_segment_and_mid_header_torn_write() {
    let mut sim = make_fault_sim(184);
    let mut out = Completions::new(16);
    let mut wal = WalEngine::new(wal_config());
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);

    // Empty segment wal_0001.log.
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot::new(0), "/wal_0001.log", rw),
        0
    );
    assert_eq!(sync_file(&mut sim, &mut out, 2, FileSlot::new(0), false), 0);
    assert_eq!(close_file(&mut sim, &mut out, 3, FileSlot::new(0)), 0);

    // Segment 2 has 1 valid record + 12-byte mid-header garbage at offset 4096.
    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 2, true, true).unwrap(),
        0
    );
    assert_eq!(
        wal.append_chained(&mut sim, &mut out, b"valid-before-tear")
            .unwrap(),
        (4096, 0)
    );
    assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            10,
            FileSlot::new(0),
            "/wal_0002.log",
            rw
        ),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 11, FileSlot::new(0), 4096, &[0xFF; 12]),
        12
    );
    assert_eq!(
        sync_file(&mut sim, &mut out, 12, FileSlot::new(0), false),
        0
    );
    assert_eq!(close_file(&mut sim, &mut out, 13, FileSlot::new(0)), 0);

    let stats = wal.recover(&mut sim, &mut out, 2, &mut |_| {}).unwrap();
    assert_eq!(stats.segments_found, 2);
    assert_eq!(stats.valid_records, 1);
    assert!(stats.stopped_on_corrupt);
}

/// `T2-F17-5`: `NOTICE` attributes UltraGraphDB (`SimStorage`) & Apache Iggy; license headers pass.
#[test]
fn t2_f17_5_notice_attribution_and_license_headers_check() {
    let root = repo_root();
    let notice = std::fs::read_to_string(root.join("NOTICE")).expect("NOTICE file must exist");
    assert!(
        notice.contains("UltraGraphDB"),
        "NOTICE must attribute UltraGraphDB: {notice}"
    );
    assert!(
        notice.contains("Iggy"),
        "NOTICE must attribute Apache Iggy: {notice}"
    );

    let out = std::process::Command::new("python3")
        .current_dir(&root)
        .args(["scripts/license-headers.py", "--check"])
        .output()
        .expect("run license-headers.py");
    assert!(
        out.status.success(),
        "license-headers.py --check failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
