// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A file opened with `dsync`: each completed write survives `power_loss`
//! whole with no `FileSync`, where the same writes to a plain file do not.

use super::{make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path, write_fill};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{BufId, Completions, FileSlot, Io, ENOENT};
use longshore_common::sim::DiskFaults;

const PATH: &str = "/dsync.dat";
const SECTOR: u32 = 512;
/// Each record spans four sectors, so a power loss can tear one.
const RECORD: u32 = 4 * SECTOR;
const RECORD_RES: i32 = 4 * 512;
const RECORDS: u32 = 4;
const SEEDS: u32 = 64;

/// What a power loss left of the records written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Left {
    whole: u32,
    torn: u32,
    lost: u32,
}

/// The byte record `idx` is filled with; never zero, which a hole reads as.
fn fill_of(idx: u32) -> u8 {
    0xA1 + idx.to_le_bytes()[0]
}

/// Creates the file with its name durable, writes the records with no
/// `FileSync`, loses power, and reports what reads back.
fn write_crash_read(seed: u64, dsync: bool) -> Left {
    let mut sim = make_sim(seed, SECTOR, 16, 64 * 1024);
    let mut out = Completions::new(16);
    let slot = FileSlot::new(0);
    let flags = rw_create().with_dsync(dsync);
    assert_eq!(open_path(&mut sim, &mut out, 1, slot, PATH, flags), 0);
    sync_dir_path(&mut sim, &mut out, 10, "/");
    let mut left = Left::default();
    sealed(|| {
        for idx in 0..RECORDS {
            let off = u64::from(idx * RECORD);
            let tok = 20 + u64::from(idx);
            let wrote = write_fill(&mut sim, &mut out, tok, slot, off, RECORD, fill_of(idx));
            assert_eq!(wrote, RECORD_RES);
        }
        sim.power_loss();
        assert_eq!(open_path(&mut sim, &mut out, 30, slot, PATH, rw_open()), 0);
        let n = read_at(&mut sim, &mut out, 31, slot, 0, RECORDS * RECORD);
        let len = usize::try_from(n).unwrap_or(0);
        assert!(n >= 0, "read after power loss: {n}");
        let got = &sim.buf(BufId::new(2))[..len];
        for idx in 0..RECORDS {
            let rec = got.get((idx * RECORD) as usize..).unwrap_or(&[]);
            let rec = &rec[..rec.len().min(RECORD as usize)];
            let kept = rec.iter().take_while(|&&b| b == fill_of(idx)).count();
            if kept == RECORD as usize {
                left.whole += 1;
            } else if kept == 0 {
                left.lost += 1;
            } else {
                assert_eq!(kept % SECTOR as usize, 0, "seed {seed}: torn in a sector");
                left.torn += 1;
            }
        }
    });
    left
}

#[test]
fn plain_writes_without_sync_are_lost_and_torn_by_power_loss() {
    let mut sum = Left::default();
    for seed in 0..u64::from(SEEDS) {
        let left = write_crash_read(seed, false);
        assert_eq!(left, write_crash_read(seed, false), "seed {seed} replays");
        sum.whole += left.whole;
        sum.torn += left.torn;
        sum.lost += left.lost;
    }
    assert_eq!(sum.whole + sum.torn + sum.lost, SEEDS * RECORDS);
    assert!(sum.lost > 0 && sum.torn > 0 && sum.whole > 0, "{sum:?}");
}

#[test]
fn dsync_writes_are_all_whole_after_power_loss_with_no_sync() {
    let all = Left {
        whole: RECORDS,
        torn: 0,
        lost: 0,
    };
    for seed in 0..u64::from(SEEDS) {
        assert_eq!(write_crash_read(seed, true), all, "seed {seed}");
    }
}

#[test]
fn dsync_overwrite_replaces_durable_data_whole() {
    for seed in 0..u64::from(SEEDS) {
        let mut sim = make_sim(seed, SECTOR, 16, 64 * 1024);
        let mut out = Completions::new(16);
        let slot = FileSlot::new(0);
        let flags = rw_create().with_dsync(true);
        assert_eq!(open_path(&mut sim, &mut out, 1, slot, PATH, flags), 0);
        sync_dir_path(&mut sim, &mut out, 10, "/");
        let first = write_fill(&mut sim, &mut out, 20, slot, 0, RECORD, 0x11);
        assert_eq!(first, RECORD_RES);
        let second = write_fill(&mut sim, &mut out, 21, slot, 0, RECORD, 0x22);
        assert_eq!(second, RECORD_RES);
        sim.power_loss();
        assert_eq!(open_path(&mut sim, &mut out, 30, slot, PATH, rw_open()), 0);
        assert_eq!(read_at(&mut sim, &mut out, 31, slot, 0, RECORD), RECORD_RES);
        let got = &sim.buf(BufId::new(2))[..RECORD as usize];
        assert!(got.iter().all(|&b| b == 0x22), "seed {seed}");
    }
}

#[test]
fn dsync_does_not_make_an_unsynced_name_durable() {
    let mut sim = make_sim(7, SECTOR, 16, 64 * 1024);
    let mut out = Completions::new(16);
    let slot = FileSlot::new(0);
    let flags = rw_create().with_dsync(true);
    assert_eq!(open_path(&mut sim, &mut out, 1, slot, PATH, flags), 0);
    let wrote = write_fill(&mut sim, &mut out, 20, slot, 0, RECORD, 0x33);
    assert_eq!(wrote, RECORD_RES);
    sim.power_loss();
    let reopened = open_path(&mut sim, &mut out, 30, slot, PATH, rw_open());
    assert_eq!(reopened, -ENOENT);
}

#[test]
fn dsync_write_fails_when_its_sync_fails_and_the_data_is_not_durable() {
    let mut sim = make_sim(3, SECTOR, 16, 64 * 1024);
    let mut out = Completions::new(16);
    let slot = FileSlot::new(0);
    let flags = rw_create().with_dsync(true);
    assert_eq!(open_path(&mut sim, &mut out, 1, slot, PATH, flags), 0);
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
    let wrote = write_fill(&mut sim, &mut out, 20, slot, 0, RECORD, 0x44);
    assert_eq!(wrote, -EIO);
    assert_eq!(sim.disk_fault_counters().sync_fail(), 1);
    sim.set_disk_faults(DiskFaults::default());
    sim.power_loss();
    assert_eq!(open_path(&mut sim, &mut out, 30, slot, PATH, rw_open()), 0);
    assert_eq!(read_at(&mut sim, &mut out, 31, slot, 0, RECORD), 0);
}
