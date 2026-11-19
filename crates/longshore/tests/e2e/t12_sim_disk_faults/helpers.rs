// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for Features 16 & 17 (`t12_sim_disk_faults`).

use super::super::harness::{
    read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes, WalConfig,
};
use longshore::io::{BufId, Completions, FileSlot, Limits, Op, OpenFlags, Token};
use longshore::sim::{DiskConfig, Faults, SimIo};
use std::path::PathBuf;

pub const LINUX_EIO: i32 = 5;
pub const LINUX_ENOSPC: i32 = 28;
pub const LINUX_EOPNOTSUPP: i32 = 95;

pub fn fault_sim_limits() -> Limits {
    Limits {
        sockets_max: 4,
        buffers: 6,
        buffer_size: 16_384,
        small_buffers: 2,
        small_buffer_size: 512,
        files_max: 8,
        aligned_buffers: 6,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 32,
        ..Limits::BASE
    }
}

pub fn wal_config() -> WalConfig {
    let lim = fault_sim_limits();
    let mut cfg = WalConfig::new("", 4096);
    cfg.write_buf = lim.aligned(0);
    cfg.read_buf = lim.aligned(1);
    cfg.path_buf = lim.small(0);
    cfg.scratch_buf = lim.small(1);
    cfg
}

pub fn make_fault_sim(seed: u64) -> SimIo {
    let mut sim = SimIo::new(fault_sim_limits(), seed, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 32,
        max_dirs: 16,
        max_bytes_per_file: 1024 * 1024,
        max_name_len: 128,
        write_log_entries: 64,
        write_log_bytes: 256 * 1024,
        read_error_sectors: 32,
        sector_size: 512,
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

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}
