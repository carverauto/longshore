// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Directory sync, cross-directory rename, and open-inode survival tests.

use super::{
    dir_open, drive, make_sim, open_path, put_path, read_at, rw_create, rw_open, sync_dir_path,
    sync_slot, write_fill,
};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::{EIO, ENOENT};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskFaults, SimIo};

#[path = "dirs_causal_order.rs"]
mod causal_order;
#[path = "dirs_enametoolong.rs"]
mod enametoolong;
#[path = "dirs_overflow.rs"]
mod overflow;
#[path = "dirs_reclaim.rs"]
mod reclaim;
#[path = "dirs_sync_fail.rs"]
mod sync_fail;

#[test]
fn open_file_survives_unlink_and_rename_and_cross_dir_rename_sync() {
    let mut sim = make_sim(5, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    mkdir_path(&mut sim, &mut out, 1, "/d1");
    mkdir_path(&mut sim, &mut out, 2, "/d2");
    sync_dir_path(&mut sim, &mut out, 10, "/");

    assert_eq!(
        open_path(&mut sim, &mut out, 20, FileSlot(0), "/d1/f", rw_create()),
        0
    );
    sim.buf(BufId(1))[..5].copy_from_slice(b"alive");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            21,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 5,
                file_off: 0,
            },
        ),
        5
    );
    assert_eq!(sync_slot(&mut sim, &mut out, 22, FileSlot(0)), 0);
    sync_dir_path(&mut sim, &mut out, 30, "/d1");

    rename_path(&mut sim, &mut out, 40, "/d1/f", "/d2/g");
    assert_slot0_reads_alive(&mut sim, &mut out, 41);

    unlink_path(&mut sim, &mut out, 42, "/d2/g");
    assert_eq!(status_of(&mut sim, &mut out, 43, "/d2/g"), -ENOENT);
    assert_slot0_reads_alive(&mut sim, &mut out, 44);
    assert_eq!(
        drive(&mut sim, &mut out, 45, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    sync_dir_path(&mut sim, &mut out, 50, "/d1");
    sync_dir_path(&mut sim, &mut out, 60, "/d2");
    sim.power_loss();
    assert_eq!(status_of(&mut sim, &mut out, 70, "/d1/f"), -ENOENT);
}

/// The still-open slot 0 reads back `alive`.
fn assert_slot0_reads_alive(sim: &mut SimIo, out: &mut Completions, tok: u64) {
    assert_eq!(read_at(sim, out, tok, FileSlot(0), 0, 5), 5);
    assert_eq!(&sim.buf(BufId(2))[..5], b"alive");
}

/// `FileStatus` of `path` into buffer 1; returns the raw result.
fn status_of(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> i32 {
    let len = put_path(sim, BufId(0), path);
    drive(
        sim,
        out,
        tok,
        Op::FileStatus {
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
            out_buf: BufId(1),
            out_off: 0,
        },
    )
}

pub(super) fn fail_dir_sync(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(open_path(sim, out, tok, FileSlot(3), path, dir_open()), 0);
    sim.set_disk_faults(DiskFaults {
        sync_fail: 1_000_000,
        ..Default::default()
    });
    assert_eq!(sync_slot(sim, out, tok + 1, FileSlot(3)), -EIO);
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(sim, out, tok + 2, Op::FileClose { slot: FileSlot(3) }),
        0
    );
}

pub(super) fn rename_path(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    old_p: &str,
    new_p: &str,
) {
    let ol = put_path(sim, BufId(0), old_p);
    let nl = put_path(sim, BufId(1), new_p);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len: ol,
                new_buf: BufId(1),
                new_off: 0,
                new_len: nl,
            },
        ),
        0
    );
}

pub(super) fn unlink_path(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    let ul = put_path(sim, BufId(0), path);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileUnlink {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ul,
            },
        ),
        0
    );
}

pub(super) fn mkdir_path(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    let len = put_path(sim, BufId(0), path);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: len,
            },
        ),
        0
    );
}

/// Creates `path` in slot 0, writes 16 bytes of `byte`, syncs and closes it (tokens `tok..tok+4`).
pub(super) fn create_write_sync(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    path: &str,
    byte: u8,
) {
    assert_eq!(open_path(sim, out, tok, FileSlot(0), path, rw_create()), 0);
    assert_eq!(write_fill(sim, out, tok + 1, FileSlot(0), 0, 16, byte), 16);
    assert_eq!(sync_slot(sim, out, tok + 2, FileSlot(0)), 0);
    close_slot0(sim, out, tok + 3);
}

/// Creates an empty `path` in slot 0 and closes it (tokens `tok`, `tok + 1`).
pub(super) fn create_empty(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(open_path(sim, out, tok, FileSlot(0), path, rw_create()), 0);
    close_slot0(sim, out, tok + 1);
}

pub(super) fn close_slot0(sim: &mut SimIo, out: &mut Completions, tok: u64) {
    assert_eq!(drive(sim, out, tok, Op::FileClose { slot: FileSlot(0) }), 0);
}

/// Opens `path` in slot 0 and checks its first 16 bytes are `byte`, leaving it open.
pub(super) fn assert_open_reads(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    path: &str,
    byte: u8,
) {
    assert_eq!(open_path(sim, out, tok, FileSlot(0), path, rw_open()), 0);
    assert_eq!(read_at(sim, out, tok + 1, FileSlot(0), 0, 16), 16);
    assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == byte));
}

/// [`assert_open_reads`], then closes slot 0 (tokens `tok..tok+3`).
pub(super) fn assert_file_byte(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    path: &str,
    byte: u8,
) {
    assert_open_reads(sim, out, tok, path, byte);
    close_slot0(sim, out, tok + 2);
}

/// `n` renames bouncing between `a` and `b`, starting `a -> b`, tokens from `tok`.
pub(super) fn ping_pong(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    n: u64,
    a: &str,
    b: &str,
) {
    for i in 0..n {
        let (old_p, new_p) = if i % 2 == 0 { (a, b) } else { (b, a) };
        rename_path(sim, out, tok + i, old_p, new_p);
    }
}

/// Opening `path` read-write in slot 1 fails with `-ENOENT`.
pub(super) fn assert_missing(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(
        open_path(sim, out, tok, FileSlot(1), path, rw_open()),
        -ENOENT
    );
}

#[test]
fn dir_sync_fail_poisoning_sibling_does_not_block_later_synced_sibling() {
    let mut sim = make_sim(17, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    sealed(|| {
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot(0), "/f1", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 2, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 10, "/");

        assert_eq!(
            open_path(&mut sim, &mut out, 20, FileSlot(0), "/f2", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 21, FileSlot(0), 0, 5, 0x42),
            5
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 22, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 23, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 30, "/");

        fail_dir_sync(&mut sim, &mut out, 40, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 50, FileSlot(0), "/f1", rw_open()),
            -ENOENT
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 51, FileSlot(0), "/f2", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 52, FileSlot(0), 0, 5), 5);
        assert!(sim.buf(BufId(2))[..5].iter().all(|&b| b == 0x42));
    });
}
