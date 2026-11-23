// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` test proving placed faults reach all 11 file operations.

use super::{drive, make_storage, open_file, put_path, rw_create, sync_dir};
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{PlacedFault, SimIo};

/// Walks disk-op numbers and tokens in lockstep: each step places a `Fail` on the
/// next disk op, sees it hit, then retries.
struct FailEach {
    tok: u64,
    next_op: u64,
}

impl FailEach {
    /// `op` fails with `-EIO` under a placed fault, then its retry returns `ok`.
    fn fail_once_then_retry(&mut self, sim: &mut SimIo, out: &mut Completions, op: Op, ok: i32) {
        sim.place_disk_fault(self.next_op, PlacedFault::Fail);
        assert_eq!(drive(sim, out, self.tok, op), -EIO);
        assert_eq!(drive(sim, out, self.tok + 1, op), ok);
        self.tok += 2;
        self.next_op += 2;
    }
}

fn path_op(sim: &mut SimIo, path: &str, make: fn(u32) -> Op) -> Op {
    make(put_path(sim, BufId(0), path))
}

#[test]
fn placed_fail_reaches_all_eleven_file_ops() {
    let (mut sim, mut out, tok) = make_storage(1, 512);
    assert_eq!(
        open_file(&mut sim, &mut out, tok, FileSlot(0), "/d/f", rw_create()),
        0
    );
    sync_dir(&mut sim, &mut out, tok + 1, "/d");
    let mut steps = FailEach {
        tok: tok + 6,
        next_op: 9,
    };
    let (sim, out) = (&mut sim, &mut out);

    let open = path_op(sim, "/d/n", |path_len| Op::FileOpen {
        slot: FileSlot(1),
        path_buf: BufId(0),
        path_off: 0,
        path_len,
        flags: rw_create(),
    });
    steps.fail_once_then_retry(sim, out, open, 0);

    sim.buf(BufId(1))[..3].copy_from_slice(b"abc");
    for (op, ok) in slot0_ops() {
        steps.fail_once_then_retry(sim, out, op, ok);
    }

    let status = path_op(sim, "/d/f", |path_len| Op::FileStatus {
        path_buf: BufId(0),
        path_off: 0,
        path_len,
        out_buf: BufId(1),
        out_off: 0,
    });
    steps.fail_once_then_retry(sim, out, status, 0);
    let mkdir = path_op(sim, "/d/e", |path_len| Op::FileMkdir {
        path_buf: BufId(0),
        path_off: 0,
        path_len,
    });
    steps.fail_once_then_retry(sim, out, mkdir, 0);
    let rename = rename_n_to_m(sim);
    steps.fail_once_then_retry(sim, out, rename, 0);
    let unlink = path_op(sim, "/d/m", |path_len| Op::FileUnlink {
        path_buf: BufId(0),
        path_off: 0,
        path_len,
    });
    steps.fail_once_then_retry(sim, out, unlink, 0);

    sim.place_disk_fault(steps.next_op, PlacedFault::Fail);
    assert_eq!(
        drive(sim, out, steps.tok, Op::FileClose { slot: FileSlot(1) }),
        -EIO
    );
    assert_eq!(sim.disk_fault_counters().placed_fail, 11);
}

/// Write, read, sync, reserve and punch on slot 0 with their success results.
fn slot0_ops() -> [(Op, i32); 5] {
    let slot = FileSlot(0);
    [
        (
            Op::FileWrite {
                slot,
                buf: BufId(1),
                off: 0,
                len: 3,
                file_off: 0,
            },
            3,
        ),
        (
            Op::FileRead {
                slot,
                buf: BufId(2),
                off: 0,
                len: 3,
                file_off: 0,
            },
            3,
        ),
        (
            Op::FileSync {
                slot,
                datasync: false,
            },
            0,
        ),
        (
            Op::FileReserve {
                slot,
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
            0,
        ),
        (
            Op::FilePunch {
                slot,
                file_off: 0,
                len: 4096,
            },
            0,
        ),
    ]
}

fn rename_n_to_m(sim: &mut SimIo) -> Op {
    let old_len = put_path(sim, BufId(0), "/d/n");
    let new_len = put_path(sim, BufId(1), "/d/m");
    Op::FileRename {
        old_buf: BufId(0),
        old_off: 0,
        old_len,
        new_buf: BufId(1),
        new_off: 0,
        new_len,
    }
}
