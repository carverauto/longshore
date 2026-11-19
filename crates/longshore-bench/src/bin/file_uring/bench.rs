// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Direct I/O write-then-sync latency benchmark: linked chain vs two submissions.

use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use std::path::Path;
use std::time::Instant;

fn run_one_mode(io: &mut UringIo, buf: BufId, iters: usize, chained: bool) -> Vec<u64> {
    let mut out = Completions::new(8);
    let mut us = Vec::with_capacity(iters);
    let mut enters_total = 0u64;
    for i in 0..iters {
        let file_off = ((i % 64) * 4096) as u64;
        let e0 = io.enters();
        let t0 = Instant::now();
        if chained {
            io.submit_chain(&[
                (
                    Token(1),
                    Op::FileWrite {
                        slot: FileSlot(0),
                        buf,
                        off: 0,
                        len: 4096,
                        file_off,
                    },
                ),
                (
                    Token(2),
                    Op::FileSync {
                        slot: FileSlot(0),
                        datasync: true,
                    },
                ),
            ])
            .unwrap();
            let mut done = 0;
            while done < 2 {
                io.tick(true, &mut out).unwrap();
                while let Some(c) = out.pop() {
                    assert!(c.result >= 0, "chain op failed: {}", c.result);
                    done += 1;
                }
            }
        } else {
            io.submit(
                Token(1),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf,
                    off: 0,
                    len: 4096,
                    file_off,
                },
            )
            .unwrap();
            io.tick(true, &mut out).unwrap();
            assert_eq!(out.pop().unwrap().result, 4096);
            io.submit(
                Token(2),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            )
            .unwrap();
            io.tick(true, &mut out).unwrap();
            assert_eq!(out.pop().unwrap().result, 0);
        }
        us.push(t0.elapsed().as_nanos() as u64);
        enters_total += io.enters() - e0;
    }
    us.sort_unstable();
    let p50 = us[iters / 2] as f64 / 1000.0;
    let p95 = us[(iters * 95) / 100] as f64 / 1000.0;
    let p99 = us[(iters * 99) / 100] as f64 / 1000.0;
    let mean = us.iter().sum::<u64>() as f64 / (iters as f64 * 1000.0);
    let label = if chained { "chain" } else { "two_ops" };
    println!(
        "{{\"mode\":\"{label}\",\"iters\":{iters},\"enters_per_op\":{:.2},\"p50_us\":{p50:.1},\"p95_us\":{p95:.1},\"p99_us\":{p99:.1},\"mean_us\":{mean:.1}}}",
        enters_total as f64 / iters as f64
    );
    us
}

pub(super) fn run_direct_sync_bench(dir: &Path, iters: usize) {
    let limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 4096,
        files_max: 2,
        aligned_buffers: 1,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).expect("UringIo::new");
    let mut out = Completions::new(8);
    let path = format!("{}/bench_wal.bin\0", dir.display());
    io.buf(BufId(0))[..path.len()].copy_from_slice(path.as_bytes());
    let wbuf = limits.aligned(0);
    io.buf(wbuf).fill(0x42);

    io.submit(
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: path.len() as u32,
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
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0, "O_DIRECT open failed");

    // Pre-allocate 256 KiB so writes overwrite allocated blocks without metadata growth jitter.
    io.submit(
        Token(2),
        Op::FileReserve {
            slot: FileSlot(0),
            file_off: 0,
            len: 64 * 4096,
            keep_size: false,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);

    // Warmup
    let _ = run_one_mode(&mut io, wbuf, 20, true);
    let _ = run_one_mode(&mut io, wbuf, 20, false);

    // Measured
    let _ = run_one_mode(&mut io, wbuf, iters, false);
    let _ = run_one_mode(&mut io, wbuf, iters, true);

    io.submit(Token(99), Op::FileClose { slot: FileSlot(0) })
        .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);
}
