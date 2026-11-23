// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` torn-write, rate-fault, and deterministic replay tests.

use super::{drive, make_storage, open_file, put_path, read_all, rw_create, sync_dir, write_bytes};
use longshore_common::io::posix::errno::linux::{EIO, ENOSPC, EOPNOTSUPP};
use longshore_common::io::{file_size, BufId, FileSlot, Io, Op};
use longshore_common::sim::{DiskFaults, PlacedFault};
use std::collections::BTreeSet;

#[test]
fn torn_reported_torn_silent_short_write_and_overwrites() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let (mut sim, mut out, mut tok) = make_storage(7, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, tok, SLOT_0, "/d/f", rw_create()),
        0
    );
    tok += 1;
    sim.place_disk_fault(6, PlacedFault::TornReported);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, SLOT_0, 0, &[0xAA; 16_384]),
        -EIO
    );
    tok += 1;
    let mut c = read_all(&mut sim, &mut out, tok, "/d/f").unwrap();
    assert!(!c.is_empty() && c.len() < 16_384 && c.len().is_multiple_of(512));
    assert!(c.iter().all(|&b| b == 0xAA));

    (sim, out, tok) = make_storage(3, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, tok, SLOT_0, "/d/f", rw_create()),
        0
    );
    tok += 1;
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, SLOT_0, 0, &[0x11; 8_192]),
        8_192
    );
    tok += 1;
    sim.place_disk_fault(7, PlacedFault::TornSilent);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, SLOT_0, 0, &[0x22; 8_192]),
        8_192
    );
    tok += 1;
    c = read_all(&mut sim, &mut out, tok, "/d/f").unwrap();
    assert_eq!(c.len(), 8_192);
    let cut = c.iter().position(|&b| b == 0x11).unwrap();
    assert!(cut > 0 && cut % 512 == 0);
    assert!(c[..cut].iter().all(|&b| b == 0x22));
    assert!(c[cut..].iter().all(|&b| b == 0x11));

    (sim, out, tok) = make_storage(7, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, tok, SLOT_0, "/d/f", rw_create()),
        0
    );
    tok += 1;
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, SLOT_0, 0, &[0x11; 4_096]),
        4_096
    );
    tok += 1;
    sim.place_disk_fault(7, PlacedFault::ShortWrite);
    let kept = write_bytes(&mut sim, &mut out, tok, SLOT_0, 0, &[0xAA; 16_384]);
    assert!(kept > 0 && kept < 16_384 && kept % 512 == 0);
    tok += 1;
    c = read_all(&mut sim, &mut out, tok, "/d/f").unwrap();
    assert_eq!(c.len(), usize::try_from(kept).unwrap().max(4_096));
    assert!(c[..usize::try_from(kept).unwrap()]
        .iter()
        .all(|&b| b == 0xAA));
    assert!(c[usize::try_from(kept).unwrap()..]
        .iter()
        .all(|&b| b == 0x11));
}

#[test]
fn sub_sector_and_one_byte_writes_never_tear_mid_sector() {
    let (mut sim, mut out, mut tok) = make_storage(5, 512);
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
    sim.place_disk_fault(6, PlacedFault::TornSilent);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, &[0x33; 64]),
        64
    );
    tok += 1;
    assert_eq!(read_all(&mut sim, &mut out, tok, "/d/f"), Ok(Vec::new()));

    (sim, out, tok) = make_storage(5, 512);
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
    sim.place_disk_fault(6, PlacedFault::TornSilent);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, &[0x33]),
        1
    );
    tok += 1;
    assert_eq!(read_all(&mut sim, &mut out, tok, "/d/f"), Ok(Vec::new()));
}

#[test]
fn placed_write_faults_on_non_write_ops_act_as_fail() {
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
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"data"),
        4
    );
    tok += 1;

    for (op_num, fault) in [
        (10, PlacedFault::TornReported),
        (11, PlacedFault::TornSilent),
        (12, PlacedFault::ShortWrite),
        (13, PlacedFault::LostReported),
        (14, PlacedFault::LostSilent),
        (15, PlacedFault::MisdirectedWrite),
    ] {
        sim.place_disk_fault(op_num, fault);
        let sl = put_path(&mut sim, BufId::new(0), "/d/f");
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                tok,
                Op::FileStatus {
                    path_buf: BufId::new(0),
                    path_off: 0,
                    path_len: sl,
                    out_buf: BufId::new(1),
                    out_off: 0,
                },
            ),
            -EIO
        );
        tok += 1;
    }
    assert_eq!(sim.disk_fault_counters().placed_fail(), 6);
    let sl = put_path(&mut sim, BufId::new(0), "/d/f");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok,
            Op::FileStatus {
                path_buf: BufId::new(0),
                path_off: 0,
                path_len: sl,
                out_buf: BufId::new(1),
                out_off: 0,
            },
        ),
        0
    );
    assert_eq!(file_size(sim.buf(BufId::new(1))), 4);
}

#[test]
fn random_rates_replay_zero_stops_and_tear_invariants() {
    for sector_size in [512u32, 4096u32] {
        let mut cuts = BTreeSet::new();
        for seed in 0..20u64 {
            let l = torn_silent_len(seed, sector_size);
            assert_eq!(l, torn_silent_len(seed, sector_size));
            let _: bool = cuts.insert(l);
        }
        assert!(cuts.len() > 1);
    }

    assert_eq!(random_rate_run(11), random_rate_run(11));
    assert_ne!(random_rate_run(11), random_rate_run(12));

    stale_placed_fault_is_skipped_and_refusals_hit();
}

/// An 8-sector write under a placed `TornSilent`; returns the sector-aligned length kept.
#[cfg(test)]
fn torn_silent_len(seed: u64, sector_size: u32) -> usize {
    let (mut sim, mut out, mut tok) = make_storage(seed, sector_size);
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
    sim.place_disk_fault(6, PlacedFault::TornSilent);
    let wlen = (sector_size * 8) as usize;
    assert_eq!(
        write_bytes(
            &mut sim,
            &mut out,
            tok,
            FileSlot::new(0),
            0,
            &vec![0xAA; wlen]
        ),
        i32::try_from(wlen).unwrap()
    );
    tok += 1;
    let got = read_all(&mut sim, &mut out, tok, "/d/f").unwrap();
    assert!(!got.is_empty() && got.len() < wlen && got.len().is_multiple_of(sector_size as usize));
    got.len()
}

/// 100 writes under random write fault rates, then 20 clean ones; returns the
/// write results and the trace hash.
fn random_rate_run(seed: u64) -> (Vec<i32>, u64) {
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
    sim.set_disk_faults(
        DiskFaults::NONE
            .with_short_write(200_000)
            .with_torn_reported(200_000)
            .with_lost_reported(100_000),
    );
    let mut res = Vec::new();
    for i in 0..100u64 {
        res.push(write_bytes(
            &mut sim,
            &mut out,
            tok + i,
            FileSlot::new(0),
            0,
            &[0x55; 1024],
        ));
    }
    let c = sim.disk_fault_counters();
    assert!(c.short_write() > 0 && c.torn_reported() > 0);
    sim.set_disk_faults(DiskFaults::default());
    for i in 0..20u64 {
        assert_eq!(
            write_bytes(
                &mut sim,
                &mut out,
                tok + 200 + i,
                FileSlot::new(0),
                0,
                b"ok"
            ),
            2
        );
    }
    (res, sim.trace_hash())
}

/// A fault placed on an already-passed op number never fires; refusal rates do.
fn stale_placed_fault_is_skipped_and_refusals_hit() {
    let (mut sim, mut out, tok) = make_storage(1, 512);
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
    sim.place_disk_fault(1, PlacedFault::Fail);
    assert_eq!(
        write_bytes(&mut sim, &mut out, tok + 1, FileSlot::new(0), 0, b"ok"),
        2
    );
    assert_eq!(sim.disk_fault_counters().placed_fail(), 0);

    sim.set_disk_faults(
        DiskFaults::NONE
            .with_reserve_refused(1_000_000)
            .with_punch_refused(1_000_000),
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            tok + 2,
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
            &mut sim,
            &mut out,
            tok + 3,
            Op::FilePunch {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 4096,
            },
        ),
        -EOPNOTSUPP
    );
}
