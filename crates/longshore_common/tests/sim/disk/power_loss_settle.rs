// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Seeded write settlement tests for `power_loss`: 3-way multi-sector,
//! 2-way single-sector, overlapping writes, and deterministic replay.

use super::{
    make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot, write_fill,
};
use longshore_common::io::{BufId, Completions, FileSlot, Io};
use longshore_common::sim::SimIo;
use std::collections::BTreeSet;

/// A fresh disk with `path` created in slot 0 and its entry made durable (tokens 1, 10..13).
fn fresh_file(seed: u64, sector_size: u32, log_bytes: usize, path: &str) -> (SimIo, Completions) {
    let mut sim = make_sim(seed, sector_size, 16, log_bytes);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, FileSlot::new(0), path, rw_create()),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    (sim, out)
}

/// Power loss, then reopen `path` in slot 0 (token 30).
fn crash_and_reopen(sim: &mut SimIo, out: &mut Completions, path: &str) {
    sim.power_loss();
    assert_eq!(
        open_path(sim, out, 30, FileSlot::new(0), path, rw_open()),
        0
    );
}

/// Synced `0x11`, unsynced `0x22` over it, power loss; returns what reads back.
#[cfg(test)]
fn settle_overwrite(seed: u64, sector_size: u32, write_len: u32) -> Vec<u8> {
    let (mut sim, mut out) = fresh_file(seed, sector_size, 64 * 1024, "/t.dat");
    let full = i32::try_from(write_len).unwrap();
    assert_eq!(
        write_fill(&mut sim, &mut out, 20, FileSlot::new(0), 0, write_len, 0x11),
        full
    );
    assert_eq!(sync_slot(&mut sim, &mut out, 21, FileSlot::new(0)), 0);
    assert_eq!(
        write_fill(&mut sim, &mut out, 22, FileSlot::new(0), 0, write_len, 0x22),
        full
    );
    crash_and_reopen(&mut sim, &mut out, "/t.dat");
    assert_eq!(
        read_at(&mut sim, &mut out, 31, FileSlot::new(0), 0, write_len),
        full
    );
    sim.buf(BufId::new(2))[..write_len as usize].to_vec()
}

#[test]
fn power_loss_settles_single_write_outcomes_and_replays_deterministically() {
    for sector_size in [512u32, 4096u32] {
        let write_len = sector_size * 4;
        let mut saw_drop = false;
        let mut saw_whole = false;
        let mut cuts = BTreeSet::new();

        for seed in 0..48u64 {
            let b1 = settle_overwrite(seed, sector_size, write_len);
            let b2 = settle_overwrite(seed, sector_size, write_len);
            assert_eq!(b1, b2, "deterministic replay failed for seed {seed}");

            let prefix = b1.iter().take_while(|&&b| b == 0x22).count();
            if prefix == 0 {
                assert!(b1.iter().all(|&b| b == 0x11));
                saw_drop = true;
            } else if prefix == write_len as usize {
                assert!(b1.iter().all(|&b| b == 0x22));
                saw_whole = true;
            } else {
                assert_eq!(prefix % sector_size as usize, 0);
                assert!(b1[prefix..].iter().all(|&b| b == 0x11));
                let _: bool = cuts.insert(prefix);
            }
        }
        assert!(saw_drop && saw_whole && !cuts.is_empty());
    }
}

#[test]
fn power_loss_single_sector_never_tears_and_overlapping_writes_settle_in_order() {
    single_sector_write_is_all_or_nothing();
    overlapping_writes_settle_per_sector_in_order();
}

fn single_sector_write_is_all_or_nothing() {
    let mut saw_zero = false;
    let mut saw_full = false;
    for seed in 0..24u64 {
        let (mut sim, mut out) = fresh_file(seed, 512, 16 * 1024, "/one.dat");
        assert_eq!(
            write_fill(&mut sim, &mut out, 20, FileSlot::new(0), 0, 256, 0xAA),
            256
        );
        crash_and_reopen(&mut sim, &mut out, "/one.dat");
        let n = read_at(&mut sim, &mut out, 31, FileSlot::new(0), 0, 256);
        if n == 0 {
            saw_zero = true;
        } else {
            assert_eq!(n, 256);
            assert!(sim.buf(BufId::new(2))[..256].iter().all(|&b| b == 0xAA));
            saw_full = true;
        }
    }
    assert!(saw_zero && saw_full);
}

#[cfg(test)]
fn overlapping_writes_settle_per_sector_in_order() {
    let mut saw_11_under_22 = false;
    for seed in 0..32u64 {
        let (mut sim, mut out) = fresh_file(seed, 512, 16 * 1024, "/ov.dat");
        assert_eq!(
            write_fill(&mut sim, &mut out, 20, FileSlot::new(0), 0, 1024, 0x11),
            1024
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 21, FileSlot::new(0), 512, 1024, 0x22),
            1024
        );
        crash_and_reopen(&mut sim, &mut out, "/ov.dat");
        let n = read_at(&mut sim, &mut out, 31, FileSlot::new(0), 0, 1536);
        let got = &sim.buf(BufId::new(2))[..usize::try_from(n).unwrap()];
        for chunk in got.chunks(512) {
            assert!(chunk.iter().all(|&b| b == chunk[0]));
        }
        if n == 1536
            && got[..512].iter().all(|&b| b == 0x11)
            && got[512..1024].iter().all(|&b| b == 0x22)
        {
            saw_11_under_22 = true;
        }
    }
    assert!(saw_11_under_22);
}
