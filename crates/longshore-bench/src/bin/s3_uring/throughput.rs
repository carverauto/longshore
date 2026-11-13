// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Throughput benchmarks: whole-object PUT and GET.

use super::*;

/// What one client thread did in its measured window.
struct Run {
    bytes: u64,
    requests: u64,
    attempts: u64,
    wall: f64,
    cpu_s: f64,
    enters: u64,
    connects: u64,
    retired: u64,
}

/// What every client thread does.
#[derive(Clone, Copy)]
struct Job {
    put: bool,
    conns: u32,
    size: u32,
    secs: u64,
    checksum: Checksum,
}

/// Where the client threads go: how many, the first one's CPU if they are
/// pinned, and how far apart the CPUs of consecutive threads are.
#[derive(Clone, Copy)]
pub(super) struct Placement {
    pub threads: u32,
    pub cpu: Option<usize>,
    pub cpu_step: usize,
}

/// One complete client on the calling thread: its own ring, connections,
/// buffers and resolver. `start` is passed once the client is ready, so
/// every thread measures the same window.
fn run(job: Job, cpu: Option<usize>, thread: u32, start: &std::sync::Barrier) -> Run {
    let Job {
        put,
        conns,
        size,
        secs,
        checksum,
    } = job;
    if let Some(cpu) = cpu {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
    }
    let (mut io, mut s3, first) = setup(conns, conns, size, conns);
    let mut out = Completions::new(64);
    let keys: Vec<String> = (0..conns)
        .map(|i| format!("bench/{}-{thread}-{i}.bin", std::process::id()))
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
    start.wait();
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
    Run {
        bytes,
        requests,
        attempts,
        wall: (io.now_ns() - t0) as f64 / 1e9,
        cpu_s: thread_cpu_secs() - cpu0,
        enters: io.enters() - enters0,
        connects: s3.connects(),
        retired: s3.small_segment_retired,
    }
}

/// Run the benchmark on `place.threads` independent client threads and
/// print one line: the totals, and each thread's rate when there are several.
pub(super) fn bench(
    put: bool,
    conns: u32,
    size: u32,
    secs: u64,
    place: Placement,
    checksum: Checksum,
) {
    let job = Job {
        put,
        conns,
        size,
        secs,
        checksum,
    };
    let cpus: Vec<Option<usize>> = (0..place.threads)
        .map(|t| {
            place.cpu.map(|c| {
                (t as usize)
                    .checked_mul(place.cpu_step)
                    .and_then(|o| c.checked_add(o))
                    .filter(|&cpu| cpu < 1024)
                    .unwrap_or_else(|| die("a client thread's CPU is past 1023"))
            })
        })
        .collect();
    let start = std::sync::Arc::new(std::sync::Barrier::new(place.threads as usize));
    let workers: Vec<_> = (0..place.threads)
        .zip(cpus)
        .map(|(t, cpu)| {
            let start = start.clone();
            std::thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job, cpu, t, &start)))
                    .unwrap_or_else(|_| die("a client thread failed"))
            })
        })
        .collect();
    let runs: Vec<Run> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let gb_per_s = |r: &Run| r.bytes as f64 / 1e9 / r.wall;
    let total: f64 = runs.iter().map(gb_per_s).sum();
    let wall = runs.iter().map(|r| r.wall).fold(0.0, f64::max);
    let (requests, attempts): (u64, u64) = runs
        .iter()
        .fold((0, 0), |a, r| (a.0 + r.requests, a.1 + r.attempts));
    let cpu_s: f64 = runs.iter().map(|r| r.cpu_s).sum();
    let thread_cpu = runs.iter().map(|r| r.cpu_s / r.wall).sum::<f64>() / runs.len() as f64;
    let gb: f64 = runs.iter().map(|r| r.bytes as f64 / 1e9).sum();
    let per_thread: Vec<String> = runs.iter().map(|r| format!("{:.3}", gb_per_s(r))).collect();
    // `thread_cpu` is the busy share of one thread, averaged over the threads.
    println!(
        "{{\"cmd\":\"{}\",\"arch\":\"{}\",\"threads\":{},\"conns\":{conns},\"size\":{size},\"secs\":{wall:.2},\"requests\":{requests},\"attempts\":{attempts},\"gb_per_s\":{:.3},\"mb_per_s\":{:.1},\"thread_cpu\":{:.3},\"gb_per_cpu_s\":{:.2},\"enters_per_request\":{:.1},\"connects\":{},\"small_segment_retired\":{},\"gb_per_s_per_thread\":[{}]}}",
        match (put, checksum) { (true, Checksum::None) => "put", (true, _) => "put-crc", _ => "get" },
        std::env::consts::ARCH,
        place.threads,
        total,
        total * 1000.0,
        thread_cpu,
        gb / cpu_s.max(1e-9),
        runs.iter().map(|r| r.enters).sum::<u64>() as f64 / requests.max(1) as f64,
        runs.iter().map(|r| r.connects).sum::<u64>(),
        runs.iter().map(|r| r.retired).sum::<u64>(),
        per_thread.join(",")
    );
    std::process::exit(0);
}
