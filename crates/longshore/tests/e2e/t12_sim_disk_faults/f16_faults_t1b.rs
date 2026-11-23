// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 16 (Part B): Read/sync/reserve/punch faults, placed faults, medium flip.

use super::super::harness::{submit_and_wait_one, write_nul_path};
use super::helpers::{
    close_file, make_fault_sim, open_file, read_file_vec, sync_file, sync_root_dir, write_file,
    LINUX_EIO, LINUX_ENOSPC, LINUX_EOPNOTSUPP,
};
use longshore::io::{BufId, Completions, FileSlot, Op, OpenFlags, Token, ENOENT};
use longshore::sim::{DiskFaults, PlacedFault};

/// `T1-F16-3`: Isolate `read_error`, `read_bit_flip`, `failed_sync`, `refused_reserve`, `refused_punch`.
#[test]
fn t1_f16_3_read_error_bit_flip_sync_fail_reserve_punch_faults() {
    let mut sim = make_fault_sim(30);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/f5.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0xAA; 4096]),
        4096
    );

    sim.set_disk_faults(DiskFaults {
        sync_fail: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        sync_file(&mut sim, &mut out, 3, FileSlot(0), false),
        -LINUX_EIO
    );
    assert_eq!(sim.disk_fault_counters().sync_fail, 1);

    sim.set_disk_faults(DiskFaults {
        reserve_refused: 1_000_000,
        ..Default::default()
    });
    let res_r = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(4),
        Op::FileReserve {
            slot: FileSlot(0),
            file_off: 4096,
            len: 4096,
            keep_size: false,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(res_r.result, -LINUX_ENOSPC);
    assert_eq!(sim.disk_fault_counters().reserve_refused, 1);

    sim.set_disk_faults(DiskFaults {
        punch_refused: 1_000_000,
        ..Default::default()
    });
    let res_p = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(5),
        Op::FilePunch {
            slot: FileSlot(0),
            file_off: 0,
            len: 4096,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(res_p.result, -LINUX_EOPNOTSUPP);
    assert_eq!(sim.disk_fault_counters().punch_refused, 1);

    sim.set_disk_faults(DiskFaults {
        bit_flip: 1_000_000,
        ..Default::default()
    });
    let flipped = read_file_vec(&mut sim, &mut out, 6, FileSlot(0), 0, 512).unwrap();
    let bit_diffs: u32 = flipped.iter().map(|&b| (b ^ 0xAA).count_ones()).sum();
    assert_eq!(bit_diffs, 1);
    assert_eq!(sim.disk_fault_counters().bit_flip, 1);

    sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 7, FileSlot(0), 0, 512),
        Err(-LINUX_EIO)
    );
    assert_eq!(sim.disk_fault_counters().read_error, 1);
}

/// `T1-F16-4`: `PlacedFault::FailAfter` on op #5 (`FileRename`) applies rename and returns `-EIO`.
#[test]
fn t1_f16_4_placed_fail_after_on_rename_takes_effect_and_returns_eio() {
    let mut sim = make_fault_sim(40);
    let mut out = Completions::new(8);
    sim.place_disk_fault(5, PlacedFault::FailAfter);

    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/p_old.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, b"fail-after"),
        10
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    assert_eq!(close_file(&mut sim, &mut out, 4, FileSlot(0)), 0);

    let l_old = write_nul_path(&mut sim, BufId(0), 0, "/p_old.dat");
    let l_new = write_nul_path(&mut sim, BufId(1), 0, "/p_new.dat");
    let rn = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(5),
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
    .unwrap();
    assert_eq!(rn.result, -LINUX_EIO);

    let l_chk = write_nul_path(&mut sim, BufId(0), 0, "/p_old.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(6),
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: l_chk,
                out_buf: BufId(2),
                out_off: 0,
            },
            10_000,
        )
        .unwrap()
        .result,
        -ENOENT
    );
    let l_new_chk = write_nul_path(&mut sim, BufId(0), 0, "/p_new.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(7),
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: l_new_chk,
                out_buf: BufId(2),
                out_off: 0,
            },
            10_000,
        )
        .unwrap()
        .result,
        0
    );
    let _: i32 = sync_root_dir(&mut sim, &mut out, 8, FileSlot(1));
}

/// `T1-F16-5`: `corrupt_medium_bit` mutates durable/log bytes persistently across reads until rewritten.
#[test]
fn t1_f16_5_corrupt_medium_bit_persists_until_rewritten() {
    let mut sim = make_fault_sim(50);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/bit.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x00; 512]),
        512
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);

    sim.corrupt_medium_bit("/bit.dat", 10, 3).unwrap();
    for tok in [10u64, 11] {
        let got = read_file_vec(&mut sim, &mut out, tok, FileSlot(0), 0, 512).unwrap();
        assert_eq!(got[10], 1 << 3);
    }

    assert_eq!(
        write_file(&mut sim, &mut out, 12, FileSlot(0), 0, &[0x00; 512]),
        512
    );
    let fixed = read_file_vec(&mut sim, &mut out, 13, FileSlot(0), 0, 512).unwrap();
    assert_eq!(fixed[10], 0);
}
