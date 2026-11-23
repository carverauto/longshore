// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` durability and directory-sync tests.

use super::{drive, make_storage, open_file, put_path, read_all, rw_create, sync_dir, write_bytes};
use longshore_common::io::posix::errno::linux::ENOENT;
use longshore_common::io::{BufId, Completions, FileSlot, Op};
use longshore_common::sim::SimIo;

#[test]
fn unsynced_write_settled_by_power_loss() {
    let mut saw_revert = false;
    let mut saw_keep = false;
    for seed in 0..24u64 {
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
        sync_dir(&mut sim, &mut out, tok, "/d");
        tok += 5;
        assert_eq!(
            write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"synced"),
            6
        );
        tok += 1;
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                tok,
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: false,
                },
            ),
            0
        );
        tok += 1;
        assert_eq!(
            write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"later!!!"),
            8
        );
        tok += 1;
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                tok,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
        tok += 1;
        assert_eq!(
            read_all(&mut sim, &mut out, tok, "/d/f"),
            Ok(b"later!!!".to_vec())
        );
        tok += 30;
        sim.power_loss();
        let got = read_all(&mut sim, &mut out, tok, "/d/f").unwrap();
        if got == b"synced" {
            saw_revert = true;
        } else if got == b"later!!!" {
            saw_keep = true;
        }
    }
    assert!(saw_revert && saw_keep);
}

#[test]
fn file_sync_does_not_make_entry_durable() {
    let mut saw_lost = false;
    for seed in 0..16u64 {
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
        assert_eq!(
            write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"data"),
            4
        );
        tok += 1;
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                tok,
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: false,
                },
            ),
            0
        );
        tok += 1;
        sim.power_loss();
        if read_all(&mut sim, &mut out, tok, "/d/f") == Err(-ENOENT) {
            saw_lost = true;
        }
    }
    assert!(saw_lost);
}

#[test]
fn dir_sync_makes_entry_durable_not_contents() {
    let mut saw_empty = false;
    for seed in 0..16u64 {
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
        assert_eq!(
            write_bytes(&mut sim, &mut out, tok, FileSlot::new(0), 0, b"data"),
            4
        );
        tok += 1;
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                tok,
                Op::FileClose {
                    slot: FileSlot::new(0)
                }
            ),
            0
        );
        tok += 1;
        sync_dir(&mut sim, &mut out, tok, "/d");
        tok += 5;
        sim.power_loss();
        let got = read_all(&mut sim, &mut out, tok, "/d/f").unwrap();
        if got.is_empty() {
            saw_empty = true;
        } else {
            assert_eq!(got, b"data");
        }
    }
    assert!(saw_empty);
}

#[test]
fn rename_and_unlink_durable_only_after_dir_sync() {
    let mut saw_rename_reverted = false;
    let mut saw_unlink_reverted = false;
    for seed in 0..16u64 {
        let (mut sim, mut out, tok) = make_storage(seed, 512);
        let tok = create_durable_a(&mut sim, &mut out, tok);
        let (reverted, tok) = rename_a_to_b_until_dir_sync(&mut sim, &mut out, tok);
        saw_rename_reverted |= reverted;
        saw_unlink_reverted |= unlink_b_until_dir_sync(&mut sim, &mut out, tok);
    }
    assert!(saw_rename_reverted && saw_unlink_reverted);
}

/// `/d/a` holding `A`, file and entry both synced; returns the next token.
fn create_durable_a(sim: &mut SimIo, out: &mut Completions, mut tok: u64) -> u64 {
    assert_eq!(
        open_file(sim, out, tok, FileSlot::new(0), "/d/a", rw_create()),
        0
    );
    tok += 1;
    assert_eq!(write_bytes(sim, out, tok, FileSlot::new(0), 0, b"A"), 1);
    tok += 1;
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: false,
            },
        ),
        0
    );
    tok += 1;
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileClose {
                slot: FileSlot::new(0)
            }
        ),
        0
    );
    tok += 1;
    sync_dir(sim, out, tok, "/d");
    tok + 5
}

/// Renames `/d/a` to `/d/b`; power loss may revert it (then it is redone) until the
/// dir sync, after which `/d/b` holds `A`. Returns whether it reverted, and the next token.
#[cfg(test)]
fn rename_a_to_b_until_dir_sync(
    sim: &mut SimIo,
    out: &mut Completions,
    mut tok: u64,
) -> (bool, u64) {
    let mut reverted = false;
    assert_eq!(rename(sim, out, tok, "/d/a", "/d/b"), 0);
    tok += 1;
    sim.power_loss();
    if read_all(sim, out, tok, "/d/a") == Ok(b"A".to_vec()) {
        reverted = true;
        assert_eq!(read_all(sim, out, tok + 30, "/d/b"), Err(-ENOENT));
        tok += 60;
        assert_eq!(rename(sim, out, tok, "/d/a", "/d/b"), 0);
        tok += 1;
    } else {
        tok += 30;
    }
    sync_dir(sim, out, tok, "/d");
    tok += 5;
    sim.power_loss();
    assert_eq!(read_all(sim, out, tok, "/d/b"), Ok(b"A".to_vec()));
    (reverted, tok + 30)
}

/// Unlinks `/d/b`; power loss may revert it (then it is redone) until the dir sync,
/// after which `/d/b` is gone. Returns whether it reverted.
#[cfg(test)]
fn unlink_b_until_dir_sync(sim: &mut SimIo, out: &mut Completions, mut tok: u64) -> bool {
    let mut reverted = false;
    assert_eq!(unlink(sim, out, tok, "/d/b"), 0);
    tok += 1;
    sim.power_loss();
    if read_all(sim, out, tok, "/d/b") == Ok(b"A".to_vec()) {
        reverted = true;
        tok += 30;
        assert_eq!(unlink(sim, out, tok, "/d/b"), 0);
        tok += 1;
    } else {
        tok += 30;
    }
    sync_dir(sim, out, tok, "/d");
    tok += 5;
    sim.power_loss();
    assert_eq!(read_all(sim, out, tok, "/d/b"), Err(-ENOENT));
    reverted
}

fn rename(sim: &mut SimIo, out: &mut Completions, tok: u64, old: &str, new: &str) -> i32 {
    let old_len = put_path(sim, BufId::new(0), old);
    let new_len = put_path(sim, BufId::new(1), new);
    drive(
        sim,
        out,
        tok,
        Op::FileRename {
            old_buf: BufId::new(0),
            old_off: 0,
            old_len,
            new_buf: BufId::new(1),
            new_off: 0,
            new_len,
        },
    )
}

fn unlink(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> i32 {
    let path_len = put_path(sim, BufId::new(0), path);
    drive(
        sim,
        out,
        tok,
        Op::FileUnlink {
            path_buf: BufId::new(0),
            path_off: 0,
            path_len,
        },
    )
}
