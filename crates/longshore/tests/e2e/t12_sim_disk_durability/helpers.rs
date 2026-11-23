// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for Features 14 & 15 (`t12_sim_disk_durability`).

use super::super::harness::{
    read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use longshore::io::{BufId, Completions, FileSlot, Limits, Op, OpenFlags, Token, ENOENT};
use longshore::sim::{DiskConfig, Faults, SimIo};

pub const LINUX_EIO: i32 = 5;
pub const LINUX_ENOSPC: i32 = 28;
pub const LINUX_ENAMETOOLONG: i32 = 36;
pub const LINUX_EMFILE: i32 = 24;
pub const LINUX_EINVAL: i32 = 22;

pub fn disk_limits() -> Limits {
    Limits {
        sockets_max: 8,
        buffers: 4,
        buffer_size: 16_384,
        small_buffers: 2,
        small_buffer_size: 512,
        files_max: 8,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 32,
        ..Limits::BASE
    }
}

pub fn make_disk_sim(
    seed: u64,
    sector_size: u32,
    write_log_entries: u32,
    write_log_bytes: usize,
) -> SimIo {
    let mut sim = SimIo::new(disk_limits(), seed, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 32,
        max_dirs: 16,
        max_bytes_per_file: 1024 * 1024,
        max_name_len: 128,
        write_log_entries,
        write_log_bytes,
        read_error_sectors: 32,
        sector_size,
    });
    sim
}

pub fn open_file(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    let plen = write_nul_path(sim, BufId(0), 0, path);
    submit_and_wait_one(
        sim,
        out,
        Token(tok),
        Op::FileOpen {
            slot,
            path_buf: BufId(0),
            path_off: 0,
            path_len: plen,
            flags,
        },
        10_000,
    )
    .expect("open")
    .result
}

pub fn close_file(sim: &mut SimIo, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    submit_and_wait_one(sim, out, Token(tok), Op::FileClose { slot }, 10_000)
        .expect("close")
        .result
}

pub fn write_file(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    file_off: u64,
    data: &[u8],
) -> i32 {
    write_pool_bytes(sim, BufId(1), 0, data);
    submit_and_wait_one(
        sim,
        out,
        Token(tok),
        Op::FileWrite {
            slot,
            buf: BufId(1),
            off: 0,
            len: data.len() as u32,
            file_off,
        },
        10_000,
    )
    .expect("write")
    .result
}

pub fn read_file_vec(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    file_off: u64,
    len: u32,
) -> Result<Vec<u8>, i32> {
    let res = submit_and_wait_one(
        sim,
        out,
        Token(tok),
        Op::FileRead {
            slot,
            buf: BufId(2),
            off: 0,
            len,
            file_off,
        },
        10_000,
    )
    .expect("read")
    .result;
    if res < 0 {
        Err(res)
    } else {
        Ok(read_pool_bytes(sim, BufId(2), 0, res as usize))
    }
}

pub fn sync_file(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    datasync: bool,
) -> i32 {
    submit_and_wait_one(
        sim,
        out,
        Token(tok),
        Op::FileSync { slot, datasync },
        10_000,
    )
    .expect("sync")
    .result
}

pub fn sync_root_dir(sim: &mut SimIo, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    let flags = OpenFlags {
        read: true,
        directory: true,
        ..Default::default()
    };
    assert_eq!(open_file(sim, out, tok, slot, "/", flags), 0);
    assert_eq!(sync_file(sim, out, tok + 1, slot, false), 0);
    close_file(sim, out, tok + 2, slot)
}

pub fn stat_exists(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> bool {
    let plen = write_nul_path(sim, BufId(0), 0, path);
    let r = submit_and_wait_one(
        sim,
        out,
        Token(tok),
        Op::FileStatus {
            path_buf: BufId(0),
            path_off: 0,
            path_len: plen,
            out_buf: BufId(3),
            out_off: 0,
        },
        10_000,
    )
    .expect("status")
    .result;
    if r == -ENOENT {
        false
    } else {
        assert_eq!(r, 0, "unexpected FileStatus error {r}");
        true
    }
}
