// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-07`: Direct I/O aligned pool + large buffer counts + `submit_chain` +
//! `FileReserve` + `FilePunch` + `FileStatus` (`statx`) across backends.

use super::super::harness::{
    posix_backend, read_pool_into, submit_and_wait_one, write_nul_path, write_pool_bytes, TempDir,
};
use longshore::io::files::file_size;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, EINVAL};
use longshore::sim::{DiskConfig, Faults, SimIo};

#[cfg(target_os = "macos")]
const EOPNOTSUPP: i32 = 95;

const BLOCK: u32 = 4096;

fn direct_limits(aligned_count: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(2)
        .with_buffers(2)
        .with_buffer_size(BLOCK)
        .with_small_buffers(4)
        .with_small_buffer_size(256)
        .with_aligned_buffers(aligned_count)
        .with_aligned_buffer_size(BLOCK)
        .with_block_size(BLOCK)
        .with_files_max(4)
        .with_ops_max(16)
}

/// Submits `op` and waits up to 1000 ticks for its result.
fn wait_result<IO: Io>(io: &mut IO, out: &mut Completions, token: Token, op: Op) -> i32 {
    submit_and_wait_one(io, out, token, op, 1_000)
        .unwrap()
        .result()
}

/// Both aligned pool buffers start on a block boundary.
fn assert_block_aligned<IO: Io>(io: &mut IO, w_buf: BufId, r_buf: BufId) {
    assert_eq!(
        io.buf(w_buf).as_ptr() as usize % (BLOCK as usize),
        0,
        "aligned pool buffer 0 must be block-aligned"
    );
    assert_eq!(
        io.buf(r_buf).as_ptr() as usize % (BLOCK as usize),
        0,
        "aligned pool buffer 1 must be block-aligned"
    );
}

/// The direct-I/O open, emptying the file, into `FileSlot(0)`.
fn open_direct(path_buf: BufId, plen: u32) -> Op {
    Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf,
        path_off: 0,
        path_len: plen,
        flags: OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true)
            .with_empty(true)
            .with_direct(true),
    }
}

/// A one-block-or-less `FileWrite` to `FileSlot(0)`.
fn write_op(buf: BufId, len: u32, file_off: u64) -> Op {
    Op::FileWrite {
        slot: FileSlot::new(0),
        buf,
        off: 0,
        len,
        file_off,
    }
}

/// A one-block `FileRead` from `FileSlot(0)` at block `blk`.
fn read_block_op(buf: BufId, blk: u64) -> Op {
    Op::FileRead {
        slot: FileSlot::new(0),
        buf,
        off: 0,
        len: BLOCK,
        file_off: u64::from(BLOCK) * blk,
    }
}

/// Writes 8 aligned 4 KiB blocks (block `n` filled with `0x30 + n`), each
/// via a chained `[FileWrite, FileSync]`.
fn write_eight_synced_blocks<IO: Io>(io: &mut IO, out: &mut Completions, w_buf: BufId) {
    for blk in 0..8u64 {
        let pattern = [0x30u8.wrapping_add(blk as u8); BLOCK as usize];
        write_pool_bytes(io, w_buf, 0, &pattern);
        io.submit_chain(&[
            (
                Token::new(100 + blk * 2),
                write_op(w_buf, BLOCK, blk * u64::from(BLOCK)),
            ),
            (
                Token::new(101 + blk * 2),
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let mut w_ok = false;
        let mut s_ok = false;
        while !(w_ok && s_ok) {
            io.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if c.token().get() == 100 + blk * 2 {
                    assert_eq!(c.result(), i32::try_from(BLOCK).unwrap());
                    w_ok = true;
                } else if c.token().get() == 101 + blk * 2 {
                    assert_eq!(c.result(), 0);
                    s_ok = true;
                }
            }
        }
    }
}

/// Punches a 16 KiB hole at 8 KiB..24 KiB (blocks 2..6); returns the result
/// after checking it succeeded (or, on macOS, was unsupported).
fn punch_blocks_2_to_6<IO: Io>(io: &mut IO, out: &mut Completions) -> i32 {
    let punch = Op::FilePunch {
        slot: FileSlot::new(0),
        file_off: u64::from(BLOCK) * 2,
        len: u64::from(BLOCK) * 4,
    };
    let result = wait_result(io, out, Token::new(300), punch);
    #[cfg(target_os = "macos")]
    let punched = result == 0 || result == -EOPNOTSUPP;
    #[cfg(not(target_os = "macos"))]
    let punched = result == 0;
    assert!(punched, "unexpected FilePunch result: {result}");
    result
}

/// Reads block `blk` into `r_buf` and checks every byte equals `fill`.
fn assert_block_reads<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    r_buf: BufId,
    blk: u64,
    fill: u8,
) {
    assert_eq!(
        wait_result(io, out, token, read_block_op(r_buf, blk)),
        i32::try_from(BLOCK).unwrap()
    );
    let mut got = [!fill; BLOCK as usize];
    read_pool_into(io, r_buf, 0, &mut got);
    assert!(
        got.iter().all(|&b| b == fill),
        "block {blk} must read as {fill:#x}"
    );
}

fn verify_direct_io_punch_and_statx<IO: Io>(io: &mut IO, lim: Limits, dir_prefix: &str) {
    let mut out = Completions::new(16);
    let path_buf = lim.small(0);
    let stat_buf = lim.small(1);
    let unaligned_buf = BufId::new(0); // non-aligned pool buffer
    let w_buf = lim.aligned(0);
    let r_buf = lim.aligned(1);
    assert_block_aligned(io, w_buf, r_buf);

    let full_path = format!("{dir_prefix}/direct_punch.bin");
    let plen = write_nul_path(io, path_buf, 0, &full_path);
    assert_eq!(
        wait_result(io, &mut out, Token::new(1), open_direct(path_buf, plen)),
        0
    );

    // Misaligned buffer, and misaligned offset/length, writes on a direct-IO
    // slot must be refused with -EINVAL.
    let bad_buf = write_op(unaligned_buf, BLOCK, 0);
    assert_eq!(wait_result(io, &mut out, Token::new(2), bad_buf), -EINVAL);
    let bad_off = write_op(w_buf, 512, 512);
    assert_eq!(wait_result(io, &mut out, Token::new(3), bad_off), -EINVAL);

    // Reserve 64 KiB (16 blocks) extending file size.
    let reserve = Op::FileReserve {
        slot: FileSlot::new(0),
        file_off: 0,
        len: u64::from(BLOCK) * 16,
        keep_size: false,
    };
    assert_eq!(wait_result(io, &mut out, Token::new(4), reserve), 0);

    write_eight_synced_blocks(io, &mut out, w_buf);
    let punch = punch_blocks_2_to_6(io, &mut out);

    // FileStatus (statx) must report full 64 KiB file size unchanged by FilePunch.
    let status = Op::FileStatus {
        path_buf,
        path_off: 0,
        path_len: plen,
        out_buf: stat_buf,
        out_off: 0,
    };
    assert_eq!(wait_result(io, &mut out, Token::new(301), status), 0);
    assert!(file_size(io.buf(stat_buf)) >= u64::from(BLOCK) * 16);

    // Read block 3 (inside punched hole if punch succeeded) and block 6 (intact).
    if punch == 0 {
        assert_block_reads(io, &mut out, Token::new(302), r_buf, 3, 0);
    }
    assert_block_reads(io, &mut out, Token::new(303), r_buf, 6, 0x36);

    let close = Op::FileClose {
        slot: FileSlot::new(0),
    };
    assert_eq!(wait_result(io, &mut out, Token::new(304), close), 0);
}

#[test]
fn t4_07_direct_io_aligned_pool_chains_punch_and_statx_sim() {
    let lim = direct_limits(4);
    let mut sim = SimIo::new(lim, 77, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(4)
            .with_max_dirs(8)
            .with_max_bytes_per_file((BLOCK as usize) * 32)
            .with_max_name_len(128)
            .with_write_log_entries(16)
            .with_write_log_bytes((BLOCK as usize) * 16)
            .with_read_error_sectors(8)
            .with_sector_size(512),
    );
    verify_direct_io_punch_and_statx(&mut sim, lim, "");
}

#[cfg(unix)]
#[test]
fn t4_07_direct_io_aligned_pool_chains_punch_and_statx_posix() {
    let tmp = TempDir::new("t4_07_posix").unwrap();
    let lim = direct_limits(4);
    let mut posix = posix_backend(lim).unwrap();
    verify_direct_io_punch_and_statx(&mut posix, lim, tmp.path().to_str().unwrap());
}
