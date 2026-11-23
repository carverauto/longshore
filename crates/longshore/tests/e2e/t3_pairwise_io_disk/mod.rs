// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios (`T3-01`–`T3-10`): I/O backends, file operations,
//! linked chains, aligned pools, and simulated disk durability/faults.

pub mod part1_posix_chains;
pub mod part2_cancel_uring;
pub mod part3_parity_crash;
pub mod part4_punch_inode;

use longshore::io::{BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
#[cfg(feature = "sim")]
use longshore::sim::DiskConfig;
use std::io;

use super::harness::submit_and_wait_one;

#[cfg(feature = "sim")]
pub(super) fn disk_config() -> DiskConfig {
    DiskConfig {
        max_files: 8,
        max_dirs: 4,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: 32,
        write_log_bytes: 32 * 1024,
        read_error_sectors: 8,
        sector_size: 512,
    }
}

pub(super) fn file_limits(files_max: u32, aligned_buffers: u32, block_size: u32) -> Limits {
    Limits {
        sockets_max: 8,
        buffers: 4,
        buffer_size: 8192,
        small_buffers: 4,
        small_buffer_size: 512,
        ops_max: 16,
        files_max,
        aligned_buffers,
        aligned_buffer_size: block_size,
        block_size,
        ..Limits::BASE
    }
}

pub(super) fn write_path<IO: Io>(io: &mut IO, buf: BufId, path: &str) -> u32 {
    super::harness::write_nul_path(io, buf, 0, path)
}

pub(super) fn open_file<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    slot: FileSlot,
    path_buf: BufId,
    path: &str,
    flags: OpenFlags,
) -> io::Result<i32> {
    let path_len = write_path(io, path_buf, path);
    let op = Op::FileOpen {
        slot,
        path_buf,
        path_off: 0,
        path_len,
        flags,
    };
    Ok(submit_and_wait_one(io, out, token, op, 10_000)?.result)
}

/// Read, write and create.
pub(super) fn rw_create() -> OpenFlags {
    OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    }
}

/// Read only.
pub(super) fn read_only() -> OpenFlags {
    OpenFlags {
        read: true,
        ..Default::default()
    }
}

/// Opens `path` (staged in `BufId(4)`) into `slot` and returns the result.
pub(super) fn open_at<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: u64,
    slot: FileSlot,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    open_file(io, out, Token(token), slot, BufId(4), path, flags).unwrap()
}

/// Submits `op`, waits up to `ticks` ticks for it, and returns its result.
pub(super) fn run_op<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: u64,
    op: Op,
    ticks: usize,
) -> i32 {
    submit_and_wait_one(io, out, Token(token), op, ticks)
        .unwrap()
        .result
}

/// A write of `buf[0..len]` to `slot` at `file_off`.
pub(super) fn write_op(slot: FileSlot, buf: BufId, len: u32, file_off: u64) -> Op {
    Op::FileWrite {
        slot,
        buf,
        off: 0,
        len,
        file_off,
    }
}

/// A read of `len` bytes of `slot` at `file_off` into `buf[0..len]`.
pub(super) fn read_op(slot: FileSlot, buf: BufId, len: u32, file_off: u64) -> Op {
    Op::FileRead {
        slot,
        buf,
        off: 0,
        len,
        file_off,
    }
}

/// A sync of `slot`.
pub(super) fn sync_op(slot: FileSlot, datasync: bool) -> Op {
    Op::FileSync { slot, datasync }
}

/// The result of the completion with `token` in `cs`.
pub(super) fn result_of(cs: &[Completion], token: u64) -> i32 {
    cs.iter().find(|c| c.token == Token(token)).unwrap().result
}

/// Opens the root directory into `slot`, syncs it and closes it, with
/// tokens `token`, `token + 1` and `token + 2`; each must succeed.
pub(super) fn sync_root_dir<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: u64,
    slot: FileSlot,
) {
    let dir = OpenFlags {
        read: true,
        directory: true,
        ..Default::default()
    };
    assert_eq!(open_at(io, out, token, slot, "/", dir), 0);
    assert_eq!(run_op(io, out, token + 1, sync_op(slot, false), 1000), 0);
    assert_eq!(run_op(io, out, token + 2, Op::FileClose { slot }, 1000), 0);
}
