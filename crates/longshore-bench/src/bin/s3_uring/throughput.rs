// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Throughput benchmarks: whole-object PUT and GET.

use super::*;

pub(super) fn bench(
    put: bool,
    conns: u32,
    size: u32,
    secs: u64,
    cpu: Option<usize>,
    checksum: Checksum,
) {
    if let Some(cpu) = cpu {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
    }
    let (mut io, mut s3, first) = setup(conns, conns, size, conns);
    let mut out = Completions::new(64);
    let keys: Vec<String> = (0..conns)
        .map(|i| format!("bench/{}-{i}.bin", std::process::id()))
        .collect();
    // For GET, the objects must exist first.
    if !put {
        for i in 0..conns {
            s3.store(
                &mut io,
                i as u64,
                &keys[i as usize],
                BufId(first + i),
                size,
                Checksum::None,
            )
            .unwrap();
        }
        for _ in 0..conns {
            if wait(&mut io, &mut s3, &mut out).outcome != Outcome::Stored {
                die("could not create objects to fetch");
            }
        }
    }
    while !s3.ready() {
        turn(&mut io, &mut s3, &mut out);
    }
    let start_one = |s3: &mut S3, io: &mut UringIo, i: u32| {
        let r = if put {
            s3.store(
                io,
                i as u64,
                &keys[i as usize],
                BufId(first + i),
                size,
                checksum,
            )
        } else {
            s3.fetch(io, i as u64, &keys[i as usize], None, BufId(first + i))
        };
        if r.is_err() {
            std::process::exit(4);
        }
    };
    for i in 0..conns {
        start_one(&mut s3, &mut io, i);
    }
    let (t0, cpu0, enters0) = (io.now_ns(), thread_cpu_secs(), io.enters());
    let deadline = t0 + secs * 1000 * MS;
    let (bytes, requests, attempts) = sealed(|| {
        let (mut bytes, mut requests, mut attempts) = (0u64, 0u64, 0u64);
        while io.now_ns() < deadline {
            turn(&mut io, &mut s3, &mut out);
            while let Some(d) = s3.poll() {
                match d.outcome {
                    Outcome::Stored | Outcome::Fetched { .. } => {
                        bytes += size as u64;
                        requests += 1;
                        attempts += d.attempts as u64;
                        start_one(&mut s3, &mut io, d.token as u32);
                    }
                    _ => std::process::exit(3),
                }
            }
        }
        (bytes, requests, attempts)
    });
    let wall = (io.now_ns() - t0) as f64 / 1e9;
    let cpu_s = thread_cpu_secs() - cpu0;
    let gb = bytes as f64 / 1e9;
    println!(
        "{{\"cmd\":\"{}\",\"arch\":\"{}\",\"conns\":{conns},\"size\":{size},\"secs\":{wall:.2},\"requests\":{requests},\"attempts\":{attempts},\"gb_per_s\":{:.3},\"mb_per_s\":{:.1},\"thread_cpu\":{:.3},\"gb_per_cpu_s\":{:.2},\"enters_per_request\":{:.1},\"connects\":{},\"small_segment_retired\":{}}}",
        match (put, checksum) { (true, Checksum::None) => "put", (true, _) => "put-crc", _ => "get" },
        std::env::consts::ARCH,
        gb / wall,
        gb * 1000.0 / wall,
        cpu_s / wall,
        gb / cpu_s.max(1e-9),
        (io.enters() - enters0) as f64 / requests.max(1) as f64,
        s3.connects(),
        s3.small_segment_retired
    );
    std::process::exit(0);
}
