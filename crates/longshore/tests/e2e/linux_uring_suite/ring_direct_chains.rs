// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel `UringIo` direct I/O chains, `FilePunch`, `statx`, and `alloc_guard`.

use super::super::harness::{write_nul_path, write_pool_bytes};
use longshore::io::alloc_guard::sealed;
use longshore::io::files::file_size;
use longshore::io::posix::errno::linux::EOPNOTSUPP;
use longshore::io::uring::UringIo;
use longshore::io::{Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, EINVAL};

fn submit_wait_one_stack<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    op: Op,
    max_ticks: usize,
) -> Completion {
    io.submit(token, op).unwrap();
    for _ in 0..max_ticks {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == token {
                return c;
            }
        }
    }
    panic!("submit_wait_one_stack timed out for {token:?}");
}

#[test]
fn uring_direct_io_chain_punch_statx_sealed() {
    let base = std::env::var("LONGSHORE_TEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join(format!("uring_direct_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let lim = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 256,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        files_max: 4,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(8);
    let path_buf = lim.small(0);
    let stat_buf = lim.small(1);
    let w_buf = lim.aligned(0);
    let r_buf = lim.aligned(1);
    let fpath = dir.join("chain_direct.bin");
    let plen = write_nul_path(&mut ring, path_buf, 0, fpath.to_str().unwrap());

    // Open direct if filesystem supports O_DIRECT, else retry buffered on tmpfs.
    let mut open_res = submit_wait_one_stack(
        &mut ring,
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
        200,
    )
    .result;
    if open_res == -EINVAL {
        open_res = submit_wait_one_stack(
            &mut ring,
            &mut out,
            Token(2),
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
                    direct: false,
                    ..Default::default()
                },
            },
            200,
        )
        .result;
    }
    assert_eq!(open_res, 0);

    sealed(|| {
        write_pool_bytes(&mut ring, w_buf, 0, &[0x4Du8; 4096]);
        ring.submit_chain(&[
            (
                Token(10),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: w_buf,
                    off: 0,
                    len: 4096,
                    file_off: 0,
                },
            ),
            (
                Token(11),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let (mut w_res, mut s_res, mut gw, mut gs) = (-1, -1, false, false);
        for _ in 0..200 {
            ring.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 10 {
                    (w_res, gw) = (c.result, true);
                } else if c.token.0 == 11 {
                    (s_res, gs) = (c.result, true);
                }
            }
            if gw && gs {
                break;
            }
        }
        assert!(gw && gs);
        assert_eq!(w_res, 4096);
        assert_eq!(s_res, 0);

        let punch = submit_wait_one_stack(
            &mut ring,
            &mut out,
            Token(12),
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
            },
            200,
        );
        assert!(punch.result == 0 || punch.result == -EOPNOTSUPP);

        let st = submit_wait_one_stack(
            &mut ring,
            &mut out,
            Token(13),
            Op::FileStatus {
                path_buf,
                path_off: 0,
                path_len: plen,
                out_buf: stat_buf,
                out_off: 0,
            },
            200,
        );
        assert_eq!(st.result, 0);
        assert_eq!(file_size(ring.buf(stat_buf)), 4096);

        if punch.result == 0 {
            let rd = submit_wait_one_stack(
                &mut ring,
                &mut out,
                Token(14),
                Op::FileRead {
                    slot: FileSlot(0),
                    buf: r_buf,
                    off: 0,
                    len: 4096,
                    file_off: 0,
                },
                200,
            );
            assert_eq!(rd.result, 4096);
            assert!(ring.buf(r_buf)[..4096].iter().all(|&b| b == 0));
        }

        let _ = submit_wait_one_stack(
            &mut ring,
            &mut out,
            Token(15),
            Op::FileClose { slot: FileSlot(0) },
            200,
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}
