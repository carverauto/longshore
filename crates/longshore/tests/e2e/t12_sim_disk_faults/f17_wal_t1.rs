// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 17: Ported `SimStorage` durability & `WalEngine` recovery suite.

use super::super::harness::WalEngine;
use super::helpers::{
    close_file, make_fault_sim, open_file, read_file_vec, sync_file, sync_root_dir, wal_config,
    write_file, LINUX_EIO,
};
use longshore::io::alloc_guard::sealed;
use longshore::io::{Completions, FileSlot, OpenFlags, ECANCELED};
use longshore::sim::{DiskFaults, PlacedFault};

/// `T1-F17-1`: Ported `SimStorage` core durability parity across segments and `power_loss()`.
#[test]
fn t1_f17_1_ported_sim_storage_core_durability_parity() {
    let mut sim = make_fault_sim(171);
    let mut out = Completions::new(16);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/seg1.bin", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0xA1; 4096]),
        4096
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot(0)), 0);

    assert_eq!(
        open_file(&mut sim, &mut out, 5, FileSlot(0), "/seg2.bin", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 6, FileSlot(0), 0, &[0xB2; 4096]),
        4096
    );
    assert_eq!(sync_file(&mut sim, &mut out, 7, FileSlot(0), false), 0);
    assert_eq!(close_file(&mut sim, &mut out, 8, FileSlot(0)), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 9, FileSlot(1)), 0);

    sim.power_loss();

    assert_eq!(
        open_file(&mut sim, &mut out, 10, FileSlot(0), "/seg1.bin", rw),
        0
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 11, FileSlot(0), 0, 4096).unwrap(),
        vec![0xA1; 4096]
    );
    assert_eq!(close_file(&mut sim, &mut out, 12, FileSlot(0)), 0);

    assert_eq!(
        open_file(&mut sim, &mut out, 13, FileSlot(0), "/seg2.bin", rw),
        0
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 14, FileSlot(0), 0, 4096).unwrap(),
        vec![0xB2; 4096]
    );
    assert_eq!(close_file(&mut sim, &mut out, 15, FileSlot(0)), 0);
}

/// `T1-F17-2`: `WalEngine` append + chained sync + `power_loss()` + `recover()` restores all records.
#[test]
fn t1_f17_2_wal_append_sync_power_loss_recover_prefix() {
    let mut sim = make_fault_sim(172);
    let mut out = Completions::new(16);
    let mut wal = WalEngine::new(wal_config());

    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot(2)), 0);

    for i in 1..=5u64 {
        let payload = format!("wal-record-{i}");
        let (w, s) = wal
            .append_chained(&mut sim, &mut out, payload.as_bytes())
            .unwrap();
        assert_eq!((w, s), (4096, 0));
    }
    assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);

    sim.power_loss();
    wal.reset_after_power_loss();

    let mut seen: Vec<(u64, Vec<u8>)> = Vec::new();
    let stats = wal
        .recover(&mut sim, &mut out, 4, &mut |rec| {
            seen.push((rec.seq, rec.payload.to_vec()));
        })
        .unwrap();
    assert_eq!(stats.valid_records, 5);
    assert_eq!(stats.last_seq, 5);
    assert!(!stats.stopped_on_corrupt && !stats.stopped_on_read_error);
    for i in 1..=5usize {
        assert_eq!(seen[i - 1].0, i as u64);
        assert_eq!(seen[i - 1].1, format!("wal-record-{i}").as_bytes());
    }
}

/// `T1-F17-3`: `WalEngine` under `short_write` and `torn_reported` recovers valid prefix and appends.
#[test]
fn t1_f17_3_wal_detects_short_and_torn_reported_tail_continues() {
    for mode in [PlacedFault::ShortWrite, PlacedFault::TornReported] {
        let mut sim = make_fault_sim(173);
        let mut out = Completions::new(16);
        let mut wal = WalEngine::new(wal_config());
        assert_eq!(
            wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
            0
        );
        assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot(2)), 0);

        for i in 1..=3u64 {
            let (w, s) = wal
                .append_chained(&mut sim, &mut out, &[i as u8; 64])
                .unwrap();
            assert_eq!((w, s), (4096, 0));
        }

        // Next append_chained submits FileWrite then FileSync; fault that FileWrite.
        sim.set_disk_faults(match mode {
            PlacedFault::ShortWrite => DiskFaults {
                short_write: 1_000_000,
                ..Default::default()
            },
            PlacedFault::Fail
            | PlacedFault::FailAfter
            | PlacedFault::TornReported
            | PlacedFault::TornSilent
            | PlacedFault::LostReported
            | PlacedFault::LostSilent
            | PlacedFault::MisdirectedWrite => DiskFaults {
                torn_reported: 1_000_000,
                ..Default::default()
            },
        });
        let (w4, s4) = wal
            .append_chained(&mut sim, &mut out, &[0x99; 3600])
            .unwrap();
        assert_eq!(s4, -ECANCELED);
        match mode {
            PlacedFault::ShortWrite => assert!(w4 > 0 && w4 < 4096),
            PlacedFault::Fail
            | PlacedFault::FailAfter
            | PlacedFault::TornReported
            | PlacedFault::TornSilent
            | PlacedFault::LostReported
            | PlacedFault::LostSilent
            | PlacedFault::MisdirectedWrite => assert_eq!(w4, -LINUX_EIO),
        }
        sim.set_disk_faults(DiskFaults::default());

        sim.power_loss();
        wal.reset_after_power_loss();

        let mut count = 0u64;
        let stats = wal
            .recover(&mut sim, &mut out, 4, &mut |rec| {
                count += 1;
                assert_eq!(rec.seq, count);
            })
            .unwrap();
        assert_eq!(stats.valid_records, 3);
        assert_eq!(stats.last_seq, 3);

        // Resume appending cleanly at offset 3 * 4096.
        wal.cursor = 3 * 4096;
        assert_eq!(
            wal.open_segment(&mut sim, &mut out, 1, false, false)
                .unwrap(),
            0
        );
        let (w_ok, s_ok) = wal.append_chained(&mut sim, &mut out, b"resumed").unwrap();
        assert_eq!((w_ok, s_ok), (4096, 0));
        assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);
    }
}

/// `T1-F17-4`: WAL CRC-64/NVME detects silent faults (`lost_silent`, `torn_silent`, `corrupt_medium_bit`).
#[test]
fn t1_f17_4_wal_crc64_detects_silent_and_bit_flip_corruption() {
    let mut sim = make_fault_sim(174);
    let mut out = Completions::new(16);
    let mut wal = WalEngine::new(wal_config());
    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
        0
    );
    assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot(2)), 0);

    for i in 1..=2u64 {
        assert_eq!(
            wal.append_chained(&mut sim, &mut out, &[i as u8; 128])
                .unwrap(),
            (4096, 0)
        );
    }
    // 3rd record is silently torn (claims 4096 written, only prefix lands -> CRC fails).
    sim.set_disk_faults(DiskFaults {
        torn_silent: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        wal.append_chained(&mut sim, &mut out, &[0xEE; 3600])
            .unwrap(),
        (4096, 0)
    );
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);

    sim.power_loss();
    wal.reset_after_power_loss();
    let stats = wal.recover(&mut sim, &mut out, 2, &mut |_| {}).unwrap();
    assert_eq!(stats.valid_records, 2);
    assert!(stats.stopped_on_corrupt);

    // Also test corrupt_medium_bit inside record #2 payload -> stops at record #1.
    sim.corrupt_medium_bit("/wal_0001.log", 4096 + 32, 2)
        .unwrap();
    let stats2 = wal.recover(&mut sim, &mut out, 2, &mut |_| {}).unwrap();
    assert_eq!(stats2.valid_records, 1);
    assert!(stats2.stopped_on_corrupt);
}

/// `T1-F17-5`: WAL append/sync/`power_loss`/recover across 20 seeds under `alloc_guard::sealed`.
#[test]
fn t1_f17_5_wal_recovery_20_seeds_zero_alloc_under_alloc_guard() {
    for seed in 0..20u64 {
        let mut sim = make_fault_sim(200 + seed);
        let mut out = Completions::new(16);
        let mut wal = WalEngine::new(wal_config());
        assert_eq!(
            wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
            0
        );
        assert_eq!(sync_root_dir(&mut sim, &mut out, 900, FileSlot(2)), 0);

        let mut payload = [0u8; 64];
        payload.fill(seed as u8);
        let mut recovered = 0u32;

        sealed(|| {
            for _ in 0..3 {
                assert_eq!(
                    wal.append_chained(&mut sim, &mut out, &payload).unwrap(),
                    (4096, 0)
                );
            }
            assert_eq!(wal.close_segment(&mut sim, &mut out).unwrap(), 0);
            sim.power_loss();
            wal.reset_after_power_loss();
            let stats = wal
                .recover(&mut sim, &mut out, 2, &mut |rec| {
                    assert_eq!(rec.payload, &payload);
                    recovered += 1;
                })
                .unwrap();
            assert_eq!(stats.valid_records, 3);
        });
        assert_eq!(recovered, 3);
    }
}
