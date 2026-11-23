// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-alloc WAL recovery helper (`wal_recover`).

use super::{drive_one, verify_record, MAX_RECS};
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags};
use longshore_common::sim::SimIo;

#[cfg(test)]
pub(crate) fn wal_recover(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: &mut u64,
    rec_bytes: usize,
    seed: u64,
) -> usize {
    let path_len = 9u32;
    assert_eq!(
        drive_one(
            sim,
            out,
            tok,
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    ..Default::default()
                },
            },
        ),
        0
    );
    let mut recovered = 0usize;
    for seq in 0..MAX_RECS as u64 {
        let file_off = seq * rec_bytes as u64;
        let r = read_record(sim, out, tok, rec_bytes, file_off);
        if r == -EIO {
            clear_unreadable_record(sim, out, tok, rec_bytes, file_off);
            break;
        }
        if r < i32::try_from(rec_bytes).unwrap() {
            break;
        }
        if verify_record(&sim.buf(BufId(2))[..rec_bytes], rec_bytes, seed, seq) {
            recovered += 1;
            continue;
        }
        let r2 = read_record(sim, out, tok, rec_bytes, file_off);
        if r2 == i32::try_from(rec_bytes).unwrap()
            && verify_record(&sim.buf(BufId(2))[..rec_bytes], rec_bytes, seed, seq)
        {
            recovered += 1;
        } else {
            break;
        }
    }
    recovered
}

/// Reads the record at `file_off` into buffer 2.
fn read_record(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: &mut u64,
    rec_bytes: usize,
    file_off: u64,
) -> i32 {
    drive_one(
        sim,
        out,
        tok,
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(2),
            off: 0,
            len: rec_bytes as u32,
            file_off,
        },
    )
}

/// Clears a record that reads back `-EIO`: a punch for whole blocks, else zeroes.
fn clear_unreadable_record(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: &mut u64,
    rec_bytes: usize,
    file_off: u64,
) {
    if rec_bytes.is_multiple_of(4096) {
        let _: i32 = drive_one(
            sim,
            out,
            tok,
            Op::FilePunch {
                slot: FileSlot(0),
                file_off,
                len: rec_bytes as u64,
            },
        );
    } else {
        sim.buf(BufId(2))[..rec_bytes].fill(0);
        let _: i32 = drive_one(
            sim,
            out,
            tok,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: rec_bytes as u32,
                file_off,
            },
        );
    }
}
