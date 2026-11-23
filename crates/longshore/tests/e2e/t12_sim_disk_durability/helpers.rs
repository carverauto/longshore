// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for Features 14 & 15 (`t12_sim_disk_durability`).

use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use longshore::io::{
    BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token, ENOENT,
};
use longshore::sim::{DiskConfig, Faults, SimIo};
use std::net::SocketAddr;

pub const LINUX_EIO: i32 = 5;
pub const LINUX_ENOSPC: i32 = 28;
pub const LINUX_ENAMETOOLONG: i32 = 36;
pub const LINUX_EMFILE: i32 = 24;
pub const LINUX_EINVAL: i32 = 22;

pub fn disk_limits() -> Limits {
    Limits::BASE
        .with_sockets_max(8)
        .with_buffers(4)
        .with_buffer_size(16_384)
        .with_small_buffers(2)
        .with_small_buffer_size(512)
        .with_files_max(8)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_ops_max(32)
}

pub fn make_disk_sim(
    seed: u64,
    sector_size: u32,
    write_log_entries: u32,
    write_log_bytes: usize,
) -> SimIo {
    let mut sim = SimIo::new(disk_limits(), seed, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(32)
            .with_max_dirs(16)
            .with_max_bytes_per_file(1024 * 1024)
            .with_max_name_len(128)
            .with_write_log_entries(write_log_entries)
            .with_write_log_bytes(write_log_bytes)
            .with_read_error_sectors(32)
            .with_sector_size(sector_size),
    );
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
    let plen = write_nul_path(sim, BufId::new(0), 0, path);
    submit_and_wait_one(
        sim,
        out,
        Token::new(tok),
        Op::FileOpen {
            slot,
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: plen,
            flags,
        },
        10_000,
    )
    .expect("open")
    .result()
}

pub fn close_file(sim: &mut SimIo, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    submit_and_wait_one(sim, out, Token::new(tok), Op::FileClose { slot }, 10_000)
        .expect("close")
        .result()
}

pub fn write_file(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    file_off: u64,
    data: &[u8],
) -> i32 {
    write_pool_bytes(sim, BufId::new(1), 0, data);
    submit_and_wait_one(
        sim,
        out,
        Token::new(tok),
        Op::FileWrite {
            slot,
            buf: BufId::new(1),
            off: 0,
            len: data.len() as u32,
            file_off,
        },
        10_000,
    )
    .expect("write")
    .result()
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
        Token::new(tok),
        Op::FileRead {
            slot,
            buf: BufId::new(2),
            off: 0,
            len,
            file_off,
        },
        10_000,
    )
    .expect("read")
    .result();
    if res < 0 {
        Err(res)
    } else {
        Ok(read_pool_bytes(
            sim,
            BufId::new(2),
            0,
            usize::try_from(res).unwrap(),
        ))
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
        Token::new(tok),
        Op::FileSync { slot, datasync },
        10_000,
    )
    .expect("sync")
    .result()
}

pub fn sync_root_dir(sim: &mut SimIo, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    let flags = OpenFlags::NONE.with_read(true).with_directory(true);
    assert_eq!(open_file(sim, out, tok, slot, "/", flags), 0);
    assert_eq!(sync_file(sim, out, tok + 1, slot, false), 0);
    close_file(sim, out, tok + 2, slot)
}

pub fn stat_exists(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> bool {
    let plen = write_nul_path(sim, BufId::new(0), 0, path);
    let r = submit_and_wait_one(
        sim,
        out,
        Token::new(tok),
        Op::FileStatus {
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: plen,
            out_buf: BufId::new(3),
            out_off: 0,
        },
        10_000,
    )
    .expect("status")
    .result();
    if r == -ENOENT {
        false
    } else {
        assert_eq!(r, 0, "unexpected FileStatus error {r}");
        true
    }
}

/// Listens on `Sock(0)` at `addr`, accepts `Sock(1)` (token `tok`) and
/// connects `Sock(2)` (token `tok + 1`); returns both completions.
pub fn listen_accept_connect(
    sim: &mut SimIo,
    out: &mut Completions,
    addr: SocketAddr,
    tok: u64,
) -> Vec<Completion> {
    let _: SocketAddr = sim.listen(Sock::new(0), addr).unwrap();
    sim.submit(
        Token::new(tok),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .unwrap();
    sim.submit(
        Token::new(tok + 1),
        Op::Connect {
            sock: Sock::new(2),
            addr,
        },
    )
    .unwrap();
    drive_until(sim, out, 100, |v| v.len() == 2).unwrap()
}
