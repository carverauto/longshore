// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-09`–`T3-10`: `read_error` + `refused_punch`
//! interaction, and open inode survival across unsynced rename + `power_loss`.

use super::super::harness::{read_pool_bytes, write_pool_bytes};
use super::{
    disk_config, file_limits, open_at, read_only, read_op, run_op, rw_create, sync_op,
    sync_root_dir, write_op, write_path,
};
use longshore::io::{BufId, Completions, FileSlot, Op, ENOENT};
use longshore::sim::{DiskFaults, Faults, SimIo};

const EIO: i32 = 5;
const EOPNOTSUPP: i32 = 95;

/// Runs `op` with only `faults` switched on, then switches them off again.
fn run_op_under(
    sim: &mut SimIo,
    out: &mut Completions,
    faults: DiskFaults,
    token: u64,
    op: Op,
) -> i32 {
    sim.set_disk_faults(faults);
    let r = run_op(sim, out, token, op, 1000);
    sim.set_disk_faults(DiskFaults::default());
    r
}

/// A punch of the second 4 KiB block of `FileSlot(0)`.
fn punch_second_block() -> Op {
    Op::FilePunch {
        slot: FileSlot(0),
        file_off: 4096,
        len: 4096,
    }
}

#[test]
fn t3_09_read_error_and_refused_punch_interaction() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 309, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);
    let read_second_block = read_op(FileSlot(0), BufId(1), 4096, 4096);

    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            1,
            FileSlot(0),
            "/t3_09.dat",
            rw_create()
        ),
        0
    );
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x55; 8192]);
    let write = write_op(FileSlot(0), BufId(0), 8192, 0);
    assert_eq!(run_op(&mut sim, &mut out, 2, write, 1000), 8192);

    let read_errors = DiskFaults {
        read_error: 1_000_000,
        ..Default::default()
    };
    let err_r = run_op_under(&mut sim, &mut out, read_errors, 3, read_second_block);
    assert_eq!(err_r, -EIO);

    let refused_punch = DiskFaults {
        punch_refused: 1_000_000,
        ..Default::default()
    };
    let p_err = run_op_under(&mut sim, &mut out, refused_punch, 4, punch_second_block());
    assert_eq!(p_err, -EOPNOTSUPP);

    let still_err = run_op(&mut sim, &mut out, 5, read_second_block, 1000);
    assert_eq!(still_err, -EIO);

    let p_ok = run_op(&mut sim, &mut out, 6, punch_second_block(), 1000);
    assert_eq!(p_ok, 0);

    let after_r = run_op(&mut sim, &mut out, 7, read_second_block, 1000);
    assert_eq!(after_r, 4096);
    assert!(read_pool_bytes(&mut sim, BufId(1), 0, 4096)
        .iter()
        .all(|&b| b == 0));
}

/// Renames `/a.dat` to `/b.dat` without syncing the directory.
fn rename_a_to_b(sim: &mut SimIo, out: &mut Completions) {
    let old_len = write_path(sim, BufId(4), "/a.dat");
    let new_len = write_path(sim, BufId(5), "/b.dat");
    let rename = Op::FileRename {
        old_buf: BufId(4),
        old_off: 0,
        old_len,
        new_buf: BufId(5),
        new_off: 0,
        new_len,
    };
    assert_eq!(run_op(sim, out, 5, rename, 1000), 0);
}

#[test]
fn t3_10_open_inode_survives_unsynced_rename_and_power_loss() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 310, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    assert_eq!(
        open_at(&mut sim, &mut out, 1, FileSlot(0), "/a.dat", rw_create()),
        0
    );
    sync_root_dir(&mut sim, &mut out, 2, FileSlot(1));
    rename_a_to_b(&mut sim, &mut out);

    let expected = [0x9A; 512];
    write_pool_bytes(&mut sim, BufId(0), 0, &expected);
    let write = write_op(FileSlot(0), BufId(0), 512, 0);
    assert_eq!(run_op(&mut sim, &mut out, 6, write, 1000), 512);
    let sync = sync_op(FileSlot(0), true);
    assert_eq!(run_op(&mut sim, &mut out, 7, sync, 1000), 0);

    sim.power_loss();
    let a_ok = open_at(&mut sim, &mut out, 8, FileSlot(0), "/a.dat", read_only());
    let b_ok = open_at(&mut sim, &mut out, 9, FileSlot(1), "/b.dat", read_only());
    assert_ne!(
        a_ok == 0,
        b_ok == 0,
        "exactly one of /a.dat or /b.dat must exist: a={a_ok} b={b_ok}"
    );
    let slot = if a_ok == 0 { FileSlot(0) } else { FileSlot(1) };
    let r = run_op(
        &mut sim,
        &mut out,
        10,
        read_op(slot, BufId(1), 512, 0),
        1000,
    );
    assert_eq!(r, 512);
    assert_eq!(read_pool_bytes(&mut sim, BufId(1), 0, 512), expected);
    if a_ok != 0 {
        assert_eq!(a_ok, -ENOENT);
    }
}
