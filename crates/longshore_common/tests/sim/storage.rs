// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ported `SimStorage` test suite rewritten anew against `Io` + [`SimIo`].
//!
//! Tests from UltraGraphDB's `sim_storage_tests.rs` that pin SplitMix64 draw
//! numbers or read its allocating operation trace (`trace`, `injected`,
//! `list`, `truncate`) have no direct counterpart here and are documented in
//! the module/PR notes.

use longshore_common::io::alloc_guard::Guard;
use longshore_common::io::{
    file_size, BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token,
};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[global_allocator]
static ALLOC: Guard = Guard;

#[path = "storage/all_ops.rs"]
mod all_ops;
#[path = "storage/durability.rs"]
mod durability;
#[path = "storage/faults.rs"]
mod faults;
#[path = "storage/paths.rs"]
mod paths;
#[path = "storage/rates.rs"]
mod rates;

pub(crate) fn make_storage(seed: u64, sector_size: u32) -> (SimIo, Completions, u64) {
    let limits = Limits {
        buffers: 4,
        buffer_size: 32 * 1024,
        small_buffers: 2,
        small_buffer_size: 128,
        files_max: 4,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 16,
        ..Limits::BASE
    };
    let mut sim = SimIo::new(limits, seed, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 16,
        max_dirs: 8,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: 32,
        write_log_bytes: 64 * 1024,
        read_error_sectors: 16,
        sector_size,
    });
    let mut out = Completions::new(16);
    assert_eq!(mkdir(&mut sim, &mut out, 1, "/d"), 0);
    sync_dir(&mut sim, &mut out, 2, "/");
    (sim, out, 10)
}

pub(crate) fn put_path(sim: &mut SimIo, buf: BufId, path: &str) -> u32 {
    let b = path.as_bytes();
    let dst = sim.buf(buf);
    dst[..b.len()].copy_from_slice(b);
    dst[b.len()] = 0;
    (b.len() + 1) as u32
}

#[cfg(test)]
pub(crate) fn drive(sim: &mut SimIo, out: &mut Completions, tok: u64, op: Op) -> i32 {
    sim.submit(Token(tok), op).unwrap();
    for _ in 0..10_000 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(tok) {
                return c.result;
            }
        }
    }
    panic!("timed out waiting for token {tok}");
}

pub(crate) fn rw_create() -> OpenFlags {
    OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    }
}

pub(crate) fn open_file(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    let len = put_path(sim, BufId(0), path);
    drive(
        sim,
        out,
        tok,
        Op::FileOpen {
            slot,
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
            flags,
        },
    )
}

pub(crate) fn write_bytes(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    off: u64,
    bytes: &[u8],
) -> i32 {
    sim.buf(BufId(1))[..bytes.len()].copy_from_slice(bytes);
    drive(
        sim,
        out,
        tok,
        Op::FileWrite {
            slot,
            buf: BufId(1),
            off: 0,
            len: bytes.len() as u32,
            file_off: off,
        },
    )
}

#[cfg(test)]
pub(crate) fn read_all(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    path: &str,
) -> Result<Vec<u8>, i32> {
    let len = put_path(sim, BufId(0), path);
    let sr = drive(
        sim,
        out,
        tok,
        Op::FileStatus {
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
            out_buf: BufId(2),
            out_off: 0,
        },
    );
    if sr < 0 {
        return Err(sr);
    }
    let sz = file_size(sim.buf(BufId(2))) as usize;
    let or = open_file(
        sim,
        out,
        tok + 1,
        FileSlot(3),
        path,
        OpenFlags {
            read: true,
            ..Default::default()
        },
    );
    if or < 0 {
        return Err(or);
    }
    let mut res = vec![0u8; sz];
    let mut off = 0usize;
    let mut step = 0u64;
    while off < sz {
        let chunk = (sz - off).min(16 * 1024) as u32;
        let r = drive(
            sim,
            out,
            tok + 2 + step,
            Op::FileRead {
                slot: FileSlot(3),
                buf: BufId(2),
                off: 0,
                len: chunk,
                file_off: off as u64,
            },
        );
        if r <= 0 {
            break;
        }
        res[off..off + usize::try_from(r).unwrap()]
            .copy_from_slice(&sim.buf(BufId(2))[..usize::try_from(r).unwrap()]);
        off += usize::try_from(r).unwrap();
        step += 1;
    }
    assert_eq!(
        drive(sim, out, tok + 20, Op::FileClose { slot: FileSlot(3) }),
        0
    );
    Ok(res)
}

pub(crate) fn mkdir(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> i32 {
    let len = put_path(sim, BufId(0), path);
    drive(
        sim,
        out,
        tok,
        Op::FileMkdir {
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    )
}

pub(crate) fn sync_dir(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(
        open_file(
            sim,
            out,
            tok,
            FileSlot(3),
            path,
            OpenFlags {
                read: true,
                directory: true,
                ..Default::default()
            },
        ),
        0
    );
    assert_eq!(
        drive(
            sim,
            out,
            tok + 1,
            Op::FileSync {
                slot: FileSlot(3),
                datasync: false,
            },
        ),
        0
    );
    assert_eq!(
        drive(sim, out, tok + 2, Op::FileClose { slot: FileSlot(3) }),
        0
    );
}
