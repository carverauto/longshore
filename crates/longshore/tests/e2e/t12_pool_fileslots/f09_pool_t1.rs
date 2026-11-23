// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 9: Aligned pool & `<= 1 GiB` range registration.

#[cfg(unix)]
use super::super::harness::posix_backend;
use super::super::harness::write_nul_path;
#[cfg(target_os = "linux")]
use super::super::harness::TempDir;
use super::helpers::{file_limits, read_start, run_on_backends, run_op, rw_create, write_start};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op};

/// `T1-F09-1`: Every aligned buffer pointer from `Io::buf` is aligned to `block_size` (4096).
#[test]
fn t1_f09_1_aligned_buffers_are_block_aligned() {
    let limits = file_limits(4, 4, 8192, 4096);
    run_on_backends(limits, |io, _| {
        for i in 0..limits.aligned_buffers() {
            let id = limits.aligned(i);
            let slice = io.buf(id);
            assert_eq!(slice.len(), 8192);
            let ptr = slice.as_ptr() as usize;
            assert_eq!(
                ptr % limits.block_size() as usize,
                0,
                "aligned buffer {i} ({id:?}) pointer {ptr:#x} not {}-byte aligned",
                limits.block_size()
            );
        }
    });
}

/// `T1-F09-2`: Pool tiling with `block_size = 8192` and `65536` has no overlap and stays in bounds.
#[test]
fn t1_f09_2_pool_tiling_with_large_block_sizes() {
    for block_size in [4096u32, 8192, 65536] {
        let limits = Limits::BASE
            .with_sockets_max(2)
            .with_buffers(3)
            .with_buffer_size(4096)
            .with_small_buffers(5)
            .with_small_buffer_size(513)
            .with_files_max(2)
            .with_aligned_buffers(4)
            .with_aligned_buffer_size(block_size)
            .with_block_size(block_size)
            .with_ops_max(4);
        let total = limits.buffer_count();
        assert_eq!(total, 12);
        let mut spans: Vec<(usize, usize)> =
            (0..total).map(|i| limits.span(BufId::new(i))).collect();
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
        for i in 0..limits.aligned_buffers() {
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
            for i in 0..limits.aligned_buffers() {
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
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(4096)
        .with_small_buffers(16_999)
        .with_small_buffer_size(4096)
        .with_files_max(2)
        .with_aligned_buffers(1)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_ops_max(4);
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
    let path_len = write_nul_path(&mut uring, BufId::new(0), 0, &path);
    let open = Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf: BufId::new(0),
        path_off: 0,
        path_len,
        flags: rw_create(false),
    };
    assert_eq!(run_op(&mut uring, &mut out, 1, open, "open"), 0);

    let high_buf = BufId::new(16_999);
    uring.buf(high_buf)[..128].fill(0x5C);
    let write = write_start(high_buf, 128);
    assert_eq!(run_op(&mut uring, &mut out, 2, write, "write"), 128);

    let read = read_start(BufId::new(0), 128);
    assert_eq!(run_op(&mut uring, &mut out, 3, read, "read"), 128);
    assert!(uring.buf(BufId::new(0))[..128].iter().all(|&b| b == 0x5C));

    let close = Op::FileClose {
        slot: FileSlot::new(0),
    };
    assert_eq!(run_op(&mut uring, &mut out, 4, close, "close"), 0);
}

/// `T1-F09-5`: `SimIo` and `Posix` support `aligned_buffers` with `Direct I/O` reads and writes.
#[test]
fn t1_f09_5_direct_io_aligned_buffers_read_write() {
    let limits = file_limits(4, 2, 4096, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut dyn_io = super::helpers::DynIo(io);
        let mut out = Completions::new(8);
        let path = format!("{prefix}direct_ok.bin");
        let path_len = write_nul_path(&mut dyn_io, BufId::new(0), 0, &path);
        let open = Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len,
            flags: rw_create(true),
        };
        let opened = run_op(&mut dyn_io, &mut out, 1, open, "open direct");
        if opened == -super::helpers::LINUX_EINVAL {
            // Some container filesystems (e.g. tmpfs) do not support O_DIRECT on real kernels.
            return;
        }
        assert_eq!(opened, 0, "direct open failed: {opened}");

        let wbuf = limits.aligned(0);
        let rbuf = limits.aligned(1);
        for (idx, b) in dyn_io.buf(wbuf).iter_mut().enumerate() {
            *b = (idx.wrapping_mul(31) ^ 0xA5) as u8;
        }
        let write = write_start(wbuf, 4096);
        assert_eq!(
            run_op(&mut dyn_io, &mut out, 2, write, "write direct"),
            4096
        );

        let read = read_start(rbuf, 4096);
        assert_eq!(run_op(&mut dyn_io, &mut out, 3, read, "read direct"), 4096);
        let (expected, actual) = dyn_io.buf_pair(wbuf, rbuf);
        assert_eq!(actual, expected);

        let close = Op::FileClose {
            slot: FileSlot::new(0),
        };
        assert_eq!(run_op(&mut dyn_io, &mut out, 4, close, "close"), 0);
    });
}
