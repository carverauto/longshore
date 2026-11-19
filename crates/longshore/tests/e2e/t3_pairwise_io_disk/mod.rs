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

use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
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
