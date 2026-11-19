// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 11: EOF/short reads, direct-IO alignment, punch, and status errors.

use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{
    close_slot, file_ops_limits, open_slot, run_on_backends, stat_size, DynIo, LINUX_EEXIST,
    LINUX_EINVAL,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token, ENOENT};

/// `T2-F11-1`: `FileRead` at/beyond EOF returns 0; short read returns exact remaining bytes.
#[test]
fn t2_f11_1_read_at_eof_and_short_read() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}eof_short.bin");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, rw),
            0
        );

        let payload: Vec<u8> = (0..100).map(|i| i as u8).collect();
        let mut d = DynIo(io);
        write_pool_bytes(&mut d, BufId(0), 0, &payload);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(0),
                    off: 0,
                    len: 100,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result,
            100
        );

        for (tok, file_off) in [(3u64, 100u64), (4, 4096)] {
            assert_eq!(
                submit_and_wait_one(
                    &mut d,
                    &mut out,
                    Token(tok),
                    Op::FileRead {
                        slot: FileSlot(0),
                        buf: BufId(1),
                        off: 0,
                        len: 64,
                        file_off,
                    },
                    10_000,
                )
                .expect("eof read")
                .result,
                0
            );
        }

        let short = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(5),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 100,
                file_off: 60,
            },
            10_000,
        )
        .expect("short read");
        assert_eq!(short.result, 40);
        assert_eq!(read_pool_bytes(&mut d, BufId(1), 0, 40), &payload[60..100]);
        assert_eq!(close_slot(io, &mut out, 6, FileSlot(0)), 0);
    });
}

/// `T2-F11-2`: Direct-IO write with non-aligned buffer, unaligned offset, or unaligned length -> `-EINVAL`.
#[test]
fn t2_f11_2_direct_io_unaligned_refused_einval() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}direct_misaligned.bin");
        let direct_rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            direct: true,
            ..Default::default()
        };
        let open_res = open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, direct_rw);
        if open_res == -LINUX_EINVAL {
            return;
        }
        assert_eq!(open_res, 0);

        let aligned = limits.aligned(0);
        let mut d = DynIo(io);
        // 1. Buffer outside aligned pool.
        let r_buf = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 4096,
                file_off: 0,
            },
            10_000,
        )
        .expect("bad buf");
        assert_eq!(r_buf.result, -LINUX_EINVAL);

        // 2. Unaligned file_off.
        let r_off = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(3),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: aligned,
                off: 0,
                len: 4096,
                file_off: 1,
            },
            10_000,
        )
        .expect("bad off");
        assert_eq!(r_off.result, -LINUX_EINVAL);

        // 3. Unaligned length.
        let r_len = submit_and_wait_one(
            &mut d,
            &mut out,
            Token(4),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: aligned,
                off: 0,
                len: 100,
                file_off: 0,
            },
            10_000,
        )
        .expect("bad len");
        assert_eq!(r_len.result, -LINUX_EINVAL);
        assert_eq!(close_slot(io, &mut out, 5, FileSlot(0)), 0);
    });
}

/// `T2-F11-3`: Misaligned write queued behind an in-flight `FileOpen(direct=true)` -> `-EINVAL`.
#[test]
fn t2_f11_3_direct_io_write_behind_inflight_open_refused() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}direct_queued.bin");
        let mut d = DynIo(io);
        let path_len = write_nul_path(&mut d, BufId(0), 0, &path);
        d.submit(
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    direct: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
        d.submit(
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 128,
                file_off: 0,
            },
        )
        .unwrap();
        let cs = drive_until(&mut d, &mut out, 10_000, |v| {
            v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
        })
        .expect("both");
        let r_open = cs.iter().find(|c| c.token == Token(1)).unwrap().result;
        let r_write = cs.iter().find(|c| c.token == Token(2)).unwrap().result;
        assert_eq!(r_write, -LINUX_EINVAL);
        if r_open == 0 {
            assert_eq!(close_slot(io, &mut out, 3, FileSlot(0)), 0);
        }
    });
}

/// `T2-F11-4`: `FilePunch` with offset or length not a multiple of `block_size` -> `-EINVAL`.
#[test]
fn t2_f11_4_punch_off_block_boundary_einval() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}punch_bad.bin");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, rw),
            0
        );
        let mut d = DynIo(io);
        for (tok, off, len) in [(2u64, 512u64, 4096u64), (3, 4096, 1024)] {
            let c = submit_and_wait_one(
                &mut d,
                &mut out,
                Token(tok),
                Op::FilePunch {
                    slot: FileSlot(0),
                    file_off: off,
                    len,
                },
                10_000,
            )
            .expect("punch bad");
            assert_eq!(c.result, -LINUX_EINVAL);
        }
        assert_eq!(close_slot(io, &mut out, 4, FileSlot(0)), 0);
    });
}

/// `T2-F11-5`: `FileStatus` on `<256B` buffer (`-EINVAL`), missing path (`-ENOENT`), and `exclusive` (`-EEXIST`).
#[test]
fn t2_f11_5_status_small_buf_missing_and_excl_exists() {
    let limits = file_ops_limits();
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let existing = format!("{prefix}exists.dat");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &existing, rw),
            0
        );
        assert_eq!(close_slot(io, &mut out, 2, FileSlot(0)), 0);

        // FileStatus into small_buffer_size (128 < 256) -> -EINVAL.
        let mut d = DynIo(io);
        let path_len = write_nul_path(&mut d, BufId(0), 0, &existing);
        let small = limits.small(0);
        assert_eq!(
            submit_and_wait_one(
                &mut d,
                &mut out,
                Token(3),
                Op::FileStatus {
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len,
                    out_buf: small,
                    out_off: 0,
                },
                10_000,
            )
            .expect("small status")
            .result,
            -LINUX_EINVAL
        );

        let missing = format!("{prefix}absent_file.dat");
        assert_eq!(
            stat_size(io, &mut out, 4, BufId(0), BufId(1), &missing),
            Err(-ENOENT)
        );

        let excl = OpenFlags {
            read: true,
            write: true,
            create: true,
            exclusive: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 5, FileSlot(0), BufId(0), &existing, excl),
            -LINUX_EEXIST
        );
    });
}
