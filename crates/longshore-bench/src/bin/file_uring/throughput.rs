// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Direct I/O sequential throughput: one thread keeps `depth` block-sized
//! writes, then reads, in flight over a preallocated file.

use longshore::io::uring::UringIo;
use longshore::io::{Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use std::path::Path;
use std::time::Instant;

extern "C" {
    fn clock_gettime(clk: core::ffi::c_int, ts: *mut [i64; 2]) -> core::ffi::c_int;
}
const CLOCK_THREAD_CPUTIME_ID: core::ffi::c_int = 3;

fn thread_cpu_secs() -> f64 {
    let mut ts = [0i64; 2];
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

#[derive(Clone, Copy)]
pub(super) struct Shape {
    pub size: u64,
    pub block: u32,
    pub depth: u32,
    pub runs: u32,
}

fn one(io: &mut UringIo, out: &mut Completions, token: u64, op: Op, what: &str) {
    io.submit(Token(token), op).unwrap();
    io.tick(true, out).unwrap();
    let c = out.pop().unwrap();
    assert_eq!(c.result, 0, "{what} failed: {}", c.result);
}

/// One sequential pass over the file. A write pass ends with `fdatasync`,
/// inside the timed interval. Returns (seconds, thread CPU seconds).
fn pass(
    io: &mut UringIo,
    out: &mut Completions,
    limits: &Limits,
    shape: &Shape,
    write: bool,
) -> (f64, f64) {
    let block = shape.block as u64;
    let mut free: Vec<u32> = (0..shape.depth).collect();
    let (mut next, mut done) = (0u64, 0u64);
    let (t0, cpu0) = (Instant::now(), thread_cpu_secs());
    while done < shape.size {
        while next < shape.size {
            let Some(i) = free.pop() else { break };
            let (slot, buf) = (FileSlot(0), limits.aligned(i));
            let op = if write {
                Op::FileWrite {
                    slot,
                    buf,
                    off: 0,
                    len: shape.block,
                    file_off: next,
                }
            } else {
                Op::FileRead {
                    slot,
                    buf,
                    off: 0,
                    len: shape.block,
                    file_off: next,
                }
            };
            io.submit(Token(i as u64), op).unwrap();
            next += block;
        }
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result as i64, block as i64, "short or failed op");
            free.push(c.token.0 as u32);
            done += block;
        }
    }
    if write {
        let sync = Op::FileSync {
            slot: FileSlot(0),
            datasync: true,
        };
        one(io, out, u64::MAX, sync, "fdatasync");
    }
    (t0.elapsed().as_secs_f64(), thread_cpu_secs() - cpu0)
}

pub(super) fn run(dir: &Path, shape: Shape) {
    assert!(shape.size.is_multiple_of(shape.block as u64));
    let limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 4096,
        files_max: 2,
        aligned_buffers: shape.depth,
        aligned_buffer_size: shape.block,
        block_size: 4096,
        ops_max: shape.depth + 2,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).expect("UringIo::new");
    let mut out = Completions::new(shape.depth as usize + 2);
    let path = format!("{}/bench_throughput.bin\0", dir.display());
    io.buf(longshore::io::BufId(0))[..path.len()].copy_from_slice(path.as_bytes());

    // Incompressible contents, different in every buffer.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..shape.depth {
        for chunk in io.buf(limits.aligned(i)).as_chunks_mut::<8>().0 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *chunk = x.to_le_bytes();
        }
    }

    let open = Op::FileOpen {
        slot: FileSlot(0),
        path_buf: longshore::io::BufId(0),
        path_off: 0,
        path_len: path.len() as u32,
        flags: OpenFlags {
            read: true,
            write: true,
            create: true,
            direct: true,
            ..Default::default()
        },
    };
    one(&mut io, &mut out, 1, open, "O_DIRECT open");
    let reserve = Op::FileReserve {
        slot: FileSlot(0),
        file_off: 0,
        len: shape.size,
        keep_size: false,
    };
    one(&mut io, &mut out, 2, reserve, "reserve");

    // Unmeasured fill: the measured writes then overwrite written extents.
    let _ = pass(&mut io, &mut out, &limits, &shape, true);

    for run in 1..=shape.runs {
        for write in [true, false] {
            let (secs, cpu) = pass(&mut io, &mut out, &limits, &shape, write);
            println!(
                "{{\"mode\":\"{}\",\"run\":{run},\"size\":{},\"block\":{},\"depth\":{},\"secs\":{secs:.3},\"gb_per_s\":{:.2},\"thread_cpu\":{:.2}}}",
                if write { "seq_write" } else { "seq_read" },
                shape.size,
                shape.block,
                shape.depth,
                shape.size as f64 / 1e9 / secs,
                cpu / secs,
            );
        }
    }
    one(
        &mut io,
        &mut out,
        99,
        Op::FileClose { slot: FileSlot(0) },
        "close",
    );
}
