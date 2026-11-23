// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Seeded write settlement tests for `power_loss`: 3-way multi-sector,
//! 2-way single-sector, overlapping writes, and deterministic replay.

use super::{drive, make_sim, open_path, rw_create, rw_open, sync_dir_path};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use std::collections::BTreeSet;

#[test]
fn power_loss_settles_single_write_outcomes_and_replays_deterministically() {
    for sector_size in [512u32, 4096u32] {
        let write_len = sector_size * 4;
        let mut saw_drop = false;
        let mut saw_whole = false;
        let mut cuts = BTreeSet::new();

        for seed in 0..48u64 {
            let run = |s: u64| -> Vec<u8> {
                let mut sim = make_sim(s, sector_size, 16, 64 * 1024);
                let mut out = Completions::new(16);
                assert_eq!(
                    open_path(&mut sim, &mut out, 1, FileSlot(0), "/t.dat", rw_create()),
                    0
                );
                sync_dir_path(&mut sim, &mut out, 10, "/");
                sim.buf(BufId(1))[..write_len as usize].fill(0x11);
                assert_eq!(
                    drive(
                        &mut sim,
                        &mut out,
                        20,
                        Op::FileWrite {
                            slot: FileSlot(0),
                            buf: BufId(1),
                            off: 0,
                            len: write_len,
                            file_off: 0,
                        },
                    ),
                    i32::try_from(write_len).unwrap()
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
                sim.buf(BufId(1))[..write_len as usize].fill(0x22);
                assert_eq!(
                    drive(
                        &mut sim,
                        &mut out,
                        22,
                        Op::FileWrite {
                            slot: FileSlot(0),
                            buf: BufId(1),
                            off: 0,
                            len: write_len,
                            file_off: 0,
                        },
                    ),
                    i32::try_from(write_len).unwrap()
                );
                sim.power_loss();
                assert_eq!(
                    open_path(&mut sim, &mut out, 30, FileSlot(0), "/t.dat", rw_open()),
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
                            len: write_len,
                            file_off: 0,
                        },
                    ),
                    i32::try_from(write_len).unwrap()
                );
                sim.buf(BufId(2))[..write_len as usize].to_vec()
            };

            let b1 = run(seed);
            let b2 = run(seed);
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
    let mut saw_zero = false;
    let mut saw_full = false;
    for seed in 0..24u64 {
        let mut sim = make_sim(seed, 512, 16, 16 * 1024);
        let mut out = Completions::new(16);
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot(0), "/one.dat", rw_create()),
            0
        );
        sync_dir_path(&mut sim, &mut out, 10, "/");
        sim.buf(BufId(1))[..256].fill(0xAA);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                20,
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 256,
                    file_off: 0,
                },
            ),
            256
        );
        sim.power_loss();
        assert_eq!(
            open_path(&mut sim, &mut out, 30, FileSlot(0), "/one.dat", rw_open()),
            0
        );
        let n = drive(
            &mut sim,
            &mut out,
            31,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 256,
                file_off: 0,
            },
        );
        if n == 0 {
            saw_zero = true;
        } else {
            assert_eq!(n, 256);
            assert!(sim.buf(BufId(2))[..256].iter().all(|&b| b == 0xAA));
            saw_full = true;
        }
    }
    assert!(saw_zero && saw_full);

    let mut saw_11_under_22 = false;
    for seed in 0..32u64 {
        let mut sim = make_sim(seed, 512, 16, 16 * 1024);
        let mut out = Completions::new(16);
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot(0), "/ov.dat", rw_create()),
            0
        );
        sync_dir_path(&mut sim, &mut out, 10, "/");
        sim.buf(BufId(1))[..1024].fill(0x11);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                20,
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 1024,
                    file_off: 0,
                },
            ),
            1024
        );
        sim.buf(BufId(1))[..1024].fill(0x22);
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                21,
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 1024,
                    file_off: 512,
                },
            ),
            1024
        );
        sim.power_loss();
        assert_eq!(
            open_path(&mut sim, &mut out, 30, FileSlot(0), "/ov.dat", rw_open()),
            0
        );
        let n = drive(
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
        );
        let got = &sim.buf(BufId(2))[..usize::try_from(n).unwrap()];
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
