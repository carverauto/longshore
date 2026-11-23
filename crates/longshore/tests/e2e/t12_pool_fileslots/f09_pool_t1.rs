// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 9: Aligned pool & `<= 1 GiB` range registration.

#[cfg(unix)]
use super::super::harness::posix_backend;
#[cfg(target_os = "linux")]
use super::super::harness::TempDir;
use super::super::harness::{submit_and_wait_one, write_nul_path};
use super::helpers::{file_limits, run_on_backends};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};

/// `T1-F09-1`: Every aligned buffer pointer from `Io::buf` is aligned to `block_size` (4096).
#[test]
fn t1_f09_1_aligned_buffers_are_block_aligned() {
    let limits = file_limits(4, 4, 8192, 4096);
    run_on_backends(limits, |io, _| {
        for i in 0..limits.aligned_buffers {
            let id = limits.aligned(i);
            let slice = io.buf(id);
            assert_eq!(slice.len(), 8192);
            let ptr = slice.as_ptr() as usize;
            assert_eq!(
                ptr % limits.block_size as usize,
                0,
                "aligned buffer {i} ({id:?}) pointer {ptr:#x} not {}-byte aligned",
                limits.block_size
            );
        }
    });
}

/// `T1-F09-2`: Pool tiling with `block_size = 8192` and `65536` has no overlap and stays in bounds.
#[test]
fn t1_f09_2_pool_tiling_with_large_block_sizes() {
    for block_size in [4096u32, 8192, 65536] {
        let limits = Limits {
            sockets_max: 2,
            buffers: 3,
            buffer_size: 4096,
            small_buffers: 5,
            small_buffer_size: 513,
            files_max: 2,
            aligned_buffers: 4,
            aligned_buffer_size: block_size,
            block_size,
            ops_max: 4,
            ..Limits::BASE
        };
        let total = limits.buffer_count();
        assert_eq!(total, 12);
        let mut spans: Vec<(usize, usize)> = (0..total).map(|i| limits.span(BufId(i))).collect();
        spans.sort_by_key(|s| s.0);
        for w in spans.windows(2) {
            assert!(
                w[0].0 + w[0].1 <= w[1].0,
                "overlapping spans {:?} and {:?} for block_size={block_size}",
                w[0],
                w[1]
            );
        }
        let last = spans.last().expect("non-empty");
        assert!(last.0 + last.1 <= limits.pool_bytes());
        for i in 0..limits.aligned_buffers {
            let (off, len) = limits.span(limits.aligned(i));
            assert_eq!(len, block_size as usize);
            assert_eq!(
                off % block_size as usize,
                0,
                "aligned offset {off} not multiple of {block_size}"
            );
        }
        #[cfg(unix)]
        {
            let mut posix = posix_backend(limits).expect("posix");
            for i in 0..limits.aligned_buffers {
                let ptr = posix.buf(limits.aligned(i)).as_ptr() as usize;
                assert_eq!(ptr % block_size as usize, 0);
            }
        }
    }
}

/// `T1-F09-3`: `buf_pair` between aligned/aligned and small/aligned has zero aliasing.
#[test]
fn t1_f09_3_buf_pair_zero_aliasing_across_classes() {
    let limits = file_limits(2, 3, 4096, 4096);
    run_on_backends(limits, |io, _| {
        let a0 = limits.aligned(0);
        let a1 = limits.aligned(1);
        {
            let (x, y) = io.buf_pair(a0, a1);
            assert_eq!((x.len(), y.len()), (4096, 4096));
            x.fill(0xAA);
            y.fill(0x55);
            assert!(x.iter().all(|&b| b == 0xAA));
            assert!(y.iter().all(|&b| b == 0x55));
        }
        let s0 = limits.small(0);
        {
            let (x, y) = io.buf_pair(s0, a0);
            assert_eq!((x.len(), y.len()), (512, 4096));
            x.fill(0x11);
            y.fill(0x22);
            assert!(x.iter().all(|&b| b == 0x11));
            assert!(y.iter().all(|&b| b == 0x22));
        }
        assert!(io.buf(a1).iter().all(|&b| b == 0x55));
    });
}

/// `T1-F09-4`: Linux `UringIo` registers >16,384 buffers across `<= 1 GiB` ranges and uses the last.
#[cfg(target_os = "linux")]
#[test]
fn t1_f09_4_linux_over_16384_registered_buffers() {
    let limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 4096,
        small_buffers: 16_999,
        small_buffer_size: 4096,
        files_max: 2,
        aligned_buffers: 1,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut uring = match UringIo::new(limits) {
        Ok(u) => u,
        Err(e) if e.kind() == std::io::ErrorKind::OutOfMemory => {
            // Unprivileged container with tight RLIMIT_MEMLOCK: verify descriptive error.
            let msg = e.to_string();
            assert!(msg.contains("RLIMIT_MEMLOCK"), "unexpected error: {msg}");
            return;
        }
        Err(e) => panic!("UringIo::new failed: {e}"),
    };
    let dir = TempDir::new("e2e_f09_many_bufs").expect("temp dir");
    let path = format!("{}/many.bin", dir.path().display());
    let mut out = Completions::new(8);
    let path_len = write_nul_path(&mut uring, BufId(0), 0, &path);
    let open = submit_and_wait_one(
        &mut uring,
        &mut out,
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
                ..Default::default()
            },
        },
        10_000,
    )
    .expect("open");
    assert_eq!(open.result, 0);

    let high_buf = BufId(16_999);
    uring.buf(high_buf)[..128].fill(0x5C);
    let wr = submit_and_wait_one(
        &mut uring,
        &mut out,
        Token(2),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: high_buf,
            off: 0,
            len: 128,
            file_off: 0,
        },
        10_000,
    )
    .expect("write");
    assert_eq!(wr.result, 128);

    let rd = submit_and_wait_one(
        &mut uring,
        &mut out,
        Token(3),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(0),
            off: 0,
            len: 128,
            file_off: 0,
        },
        10_000,
    )
    .expect("read");
    assert_eq!(rd.result, 128);
    assert!(uring.buf(BufId(0))[..128].iter().all(|&b| b == 0x5C));

    let cl = submit_and_wait_one(
        &mut uring,
        &mut out,
        Token(4),
        Op::FileClose { slot: FileSlot(0) },
        10_000,
    )
    .expect("close");
    assert_eq!(cl.result, 0);
}

/// `T1-F09-5`: `SimIo` and `Posix` support `aligned_buffers` with `Direct I/O` reads and writes.
#[test]
fn t1_f09_5_direct_io_aligned_buffers_read_write() {
    let limits = file_limits(4, 2, 4096, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut dyn_io = super::helpers::DynIo(io);
        let mut out = Completions::new(8);
        let path = format!("{prefix}direct_ok.bin");
        let path_len = write_nul_path(&mut dyn_io, BufId(0), 0, &path);
        let open = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
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
            10_000,
        )
        .expect("open direct");
        if open.result == -super::helpers::LINUX_EINVAL {
            // Some container filesystems (e.g. tmpfs) do not support O_DIRECT on real kernels.
            return;
        }
        assert_eq!(open.result, 0, "direct open failed: {}", open.result);

        let wbuf = limits.aligned(0);
        let rbuf = limits.aligned(1);
        for (idx, b) in dyn_io.buf(wbuf).iter_mut().enumerate() {
            *b = (idx.wrapping_mul(31) ^ 0xA5) as u8;
        }
        let wr = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: wbuf,
                off: 0,
                len: 4096,
                file_off: 0,
            },
            10_000,
        )
        .expect("write direct");
        assert_eq!(wr.result, 4096);

        let rd = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(3),
            Op::FileRead {
                slot: FileSlot(0),
                buf: rbuf,
                off: 0,
                len: 4096,
                file_off: 0,
            },
            10_000,
        )
        .expect("read direct");
        assert_eq!(rd.result, 4096);
        let (expected, actual) = dyn_io.buf_pair(wbuf, rbuf);
        assert_eq!(actual, expected);

        let cl = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(4),
            Op::FileClose { slot: FileSlot(0) },
            10_000,
        )
        .expect("close");
        assert_eq!(cl.result, 0);
    });
}
