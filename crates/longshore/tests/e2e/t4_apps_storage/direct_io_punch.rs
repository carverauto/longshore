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
    Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: BLOCK,
        small_buffers: 4,
        small_buffer_size: 256,
        aligned_buffers: aligned_count,
        aligned_buffer_size: BLOCK,
        block_size: BLOCK,
        files_max: 4,
        ops_max: 16,
        ..Limits::BASE
    }
}

fn verify_direct_io_punch_and_statx<IO: Io>(io: &mut IO, lim: Limits, dir_prefix: &str) {
    let mut out = Completions::new(16);
    let path_buf = lim.small(0);
    let stat_buf = lim.small(1);
    let unaligned_buf = BufId(0); // non-aligned pool buffer
    let w_buf = lim.aligned(0);
    let r_buf = lim.aligned(1);

    // Alignment check on aligned pool pointers.
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

    let full_path = format!("{dir_prefix}/direct_punch.bin");
    let plen = write_nul_path(io, path_buf, 0, &full_path);

    let open_c = submit_and_wait_one(
        io,
        &mut out,
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf,
            path_off: 0,
            path_len: plen,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                empty: true,
                direct: true,
                ..Default::default()
            },
        },
        1_000,
    )
    .unwrap();
    assert_eq!(open_c.result, 0);

    // Misaligned buffer write on direct-IO slot must be refused with -EINVAL.
    let bad_buf_c = submit_and_wait_one(
        io,
        &mut out,
        Token(2),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: unaligned_buf,
            off: 0,
            len: BLOCK,
            file_off: 0,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(bad_buf_c.result, -EINVAL);

    // Misaligned offset/length write must be refused with -EINVAL.
    let bad_off_c = submit_and_wait_one(
        io,
        &mut out,
        Token(3),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: w_buf,
            off: 0,
            len: 512,
            file_off: 512,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(bad_off_c.result, -EINVAL);

    // Reserve 64 KiB (16 blocks) extending file size.
    let res_c = submit_and_wait_one(
        io,
        &mut out,
        Token(4),
        Op::FileReserve {
            slot: FileSlot(0),
            file_off: 0,
            len: (BLOCK as u64) * 16,
            keep_size: false,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(res_c.result, 0);

    // Write 8 aligned 4 KiB blocks via chained [FileWrite, FileSync].
    for blk in 0..8u64 {
        let pattern = [0x30u8.wrapping_add(blk as u8); BLOCK as usize];
        write_pool_bytes(io, w_buf, 0, &pattern);
        io.submit_chain(&[
            (
                Token(100 + blk * 2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: w_buf,
                    off: 0,
                    len: BLOCK,
                    file_off: blk * u64::from(BLOCK),
                },
            ),
            (
                Token(101 + blk * 2),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let mut w_ok = false;
        let mut s_ok = false;
        while !(w_ok && s_ok) {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 100 + blk * 2 {
                    assert_eq!(c.result, BLOCK as i32);
                    w_ok = true;
                } else if c.token.0 == 101 + blk * 2 {
                    assert_eq!(c.result, 0);
                    s_ok = true;
                }
            }
        }
    }

    // Punch a 16 KiB hole at offset 8 KiB..24 KiB (blocks 2..6).
    let punch_c = submit_and_wait_one(
        io,
        &mut out,
        Token(300),
        Op::FilePunch {
            slot: FileSlot(0),
            file_off: u64::from(BLOCK) * 2,
            len: u64::from(BLOCK) * 4,
        },
        1_000,
    )
    .unwrap();
    #[cfg(target_os = "macos")]
    let punched = punch_c.result == 0 || punch_c.result == -EOPNOTSUPP;
    #[cfg(not(target_os = "macos"))]
    let punched = punch_c.result == 0;
    assert!(punched, "unexpected FilePunch result: {}", punch_c.result);

    // FileStatus (statx) must report full 64 KiB file size unchanged by FilePunch.
    let st_c = submit_and_wait_one(
        io,
        &mut out,
        Token(301),
        Op::FileStatus {
            path_buf,
            path_off: 0,
            path_len: plen,
            out_buf: stat_buf,
            out_off: 0,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(st_c.result, 0);
    assert!(file_size(io.buf(stat_buf)) >= u64::from(BLOCK) * 16);

    // Read block 3 (inside punched hole if punch succeeded) and block 6 (intact).
    if punch_c.result == 0 {
        let hole_c = submit_and_wait_one(
            io,
            &mut out,
            Token(302),
            Op::FileRead {
                slot: FileSlot(0),
                buf: r_buf,
                off: 0,
                len: BLOCK,
                file_off: u64::from(BLOCK) * 3,
            },
            1_000,
        )
        .unwrap();
        assert_eq!(hole_c.result, BLOCK as i32);
        let mut got = [1u8; BLOCK as usize];
        read_pool_into(io, r_buf, 0, &mut got);
        assert!(
            got.iter().all(|&b| b == 0),
            "punched hole must read as zeros"
        );
    }

    let intact_c = submit_and_wait_one(
        io,
        &mut out,
        Token(303),
        Op::FileRead {
            slot: FileSlot(0),
            buf: r_buf,
            off: 0,
            len: BLOCK,
            file_off: u64::from(BLOCK) * 6,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(intact_c.result, BLOCK as i32);
    let mut got6 = [0u8; BLOCK as usize];
    read_pool_into(io, r_buf, 0, &mut got6);
    assert!(got6.iter().all(|&b| b == 0x36));

    let close_c = submit_and_wait_one(
        io,
        &mut out,
        Token(304),
        Op::FileClose { slot: FileSlot(0) },
        1_000,
    )
    .unwrap();
    assert_eq!(close_c.result, 0);
}

#[test]
fn t4_07_direct_io_aligned_pool_chains_punch_and_statx_sim() {
    let lim = direct_limits(4);
    let mut sim = SimIo::new(lim, 77, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 8,
        max_bytes_per_file: (BLOCK as usize) * 32,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: (BLOCK as usize) * 16,
        read_error_sectors: 8,
        sector_size: 512,
    });
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
