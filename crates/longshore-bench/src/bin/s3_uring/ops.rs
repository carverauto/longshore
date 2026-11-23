// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Request-rate benchmarks for operations where bytes per second is not the point.

use super::*;

/// Request-rate benchmarks for the operations where bytes per second is
/// not the point: a conditional store of a new key (`put-new`), a ranged
/// fetch of the first 4 KiB (`get-range`), an erase (`erase`, of keys
/// that do not exist, so nothing has to be created first), and a
/// listing of 200 keys (`list-keys`). Reports requests per second, client CPU
/// per request, and latency percentiles from a log-scale histogram.
pub(super) fn ops(kind: &str, conns: u32, size: u32, secs: u64, cpu: Option<usize>) {
    if let Some(cpu) = cpu {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        let _: i32 = unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
    }
    const LISTED: u32 = 200;
    const RANGE: u64 = 4096;
    // A listing of 200 keys is about 60 kB of XML.
    let buffer = if kind == "list-keys" {
        size.max(256 * 1024)
    } else {
        size.max(RANGE as u32)
    };
    let (mut io, mut s3, first) = setup(conns, conns, buffer, conns);
    let mut out = Completions::new(64);
    let run = format!("bench/ops-{}", std::process::id());
    // Set up what the operation needs to exist.
    let seeded = match kind {
        "get-range" => conns,
        "list-keys" => LISTED,
        _ => 0,
    };
    let seed_len = if kind == "list-keys" { 1 } else { size };
    let mut made = 0;
    while made < seeded {
        let batch = conns.min(seeded - made);
        for i in 0..batch {
            let key = if kind == "list-keys" {
                format!("{run}/list/{:05}", made + i)
            } else {
                format!("{run}/{}", made + i)
            };
            s3.store(
                &mut io,
                u64::from(i),
                &key,
                BufId(first + i),
                seed_len,
                Checksum::None,
            )
            .unwrap_or_else(|e| die(format_args!("seed: {e:?}")));
        }
        for _ in 0..batch {
            if wait(&mut io, &mut s3, &mut out).outcome != Outcome::Stored {
                die("could not create the objects this benchmark reads");
            }
        }
        made += batch;
    }
    while !s3.ready() {
        turn(&mut io, &mut s3, &mut out);
    }
    let list_prefix = format!("{run}/list/");
    let prefix = format!("{run}/");
    // Keys are written into a fixed buffer: nothing may allocate once the run starts.
    let key_for = |n: u64, buf: &mut [u8; 96]| -> usize {
        let p = prefix.as_bytes();
        buf[..p.len()].copy_from_slice(p);
        let mut digits = [0u8; 20];
        let mut at = 20;
        let mut v = n;
        loop {
            at -= 1;
            digits[at] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        buf[p.len()..p.len() + 20 - at].copy_from_slice(&digits[at..]);
        p.len() + 20 - at
    };
    let mut seq = 0u64;
    let mut started = vec![0u64; conns as usize];
    let mut start_one = |client: &mut S3, ring: &mut UringIo, i: u32, start_ns: &mut [u64]| {
        let mut key = [0u8; 96];
        let buf = BufId(first + i);
        let r = match kind {
            "put-new" => {
                seq += 1;
                let n = key_for(seq, &mut key);
                client.store_no_clobber(
                    ring,
                    u64::from(i),
                    core::str::from_utf8(&key[..n]).unwrap(),
                    buf,
                    size,
                    Checksum::None,
                )
            }
            "get-range" => {
                let n = key_for(u64::from(i), &mut key);
                client.fetch(
                    ring,
                    u64::from(i),
                    core::str::from_utf8(&key[..n]).unwrap(),
                    Some((0, RANGE.min(u64::from(size)) - 1)),
                    buf,
                )
            }
            "erase" => {
                seq += 1;
                let n = key_for(1_000_000_000 + seq, &mut key);
                client.erase(ring, u64::from(i), core::str::from_utf8(&key[..n]).unwrap())
            }
            _ => client.list(ring, u64::from(i), &list_prefix, None, 1000, buf),
        };
        if r.is_err() {
            std::process::exit(4);
        }
        start_ns[i as usize] = ring.now_ns();
    };
    for i in 0..conns {
        start_one(&mut s3, &mut io, i, &mut started);
    }
    let (t0, cpu0, enters0) = (io.now_ns(), thread_cpu_secs(), io.enters());
    let deadline = t0 + secs * 1000 * MS;
    // Latency histogram: bucket k holds requests that took [2^k, 2^(k+1)) microseconds.
    let mut hist = [0u64; 32];
    let (requests, attempts, bytes) = sealed(|| {
        let (mut requests, mut attempts, mut bytes) = (0u64, 0u64, 0u64);
        while io.now_ns() < deadline {
            turn(&mut io, &mut s3, &mut out);
            while let Some(d) = s3.poll() {
                let i = d.token as usize;
                match (kind, d.outcome) {
                    ("put-new", Outcome::Stored) => bytes += u64::from(size),
                    ("get-range", Outcome::Fetched { len, .. }) => bytes += u64::from(len),
                    ("erase", Outcome::Erased) => {}
                    ("list-keys", Outcome::Listed { len }) => bytes += u64::from(len),
                    _ => std::process::exit(3),
                }
                let us = ((io.now_ns() - started[i]) / 1000).max(1);
                hist[(63 - us.leading_zeros() as usize).min(31)] += 1;
                requests += 1;
                attempts += u64::from(d.attempts);
                start_one(&mut s3, &mut io, i as u32, &mut started);
            }
        }
        (requests, attempts, bytes)
    });
    let wall = (io.now_ns() - t0) as f64 / 1e9;
    let cpu_s = thread_cpu_secs() - cpu0;
    // The upper edge of the bucket that holds the given percentage of requests.
    let pct = |percent: u64| -> u64 {
        let want = (requests * percent).div_ceil(100);
        let mut seen = 0;
        for (k, n) in hist.iter().enumerate() {
            seen += n;
            if seen >= want.max(1) {
                return 1 << (k + 1);
            }
        }
        0
    };
    println!(
        "{{\"cmd\":\"{kind}\",\"arch\":\"{}\",\"conns\":{conns},\"size\":{size},\"secs\":{wall:.2},\"requests\":{requests},\"attempts\":{attempts},\"requests_per_s\":{:.0},\"cpu_us_per_request\":{:.1},\"thread_cpu\":{:.3},\"p50_us_below\":{},\"p99_us_below\":{},\"bytes_per_request\":{},\"enters_per_request\":{:.1},\"connects\":{}}}",
        std::env::consts::ARCH,
        requests as f64 / wall,
        cpu_s * 1e6 / requests.max(1) as f64,
        cpu_s / wall,
        pct(50),
        pct(99),
        bytes / requests.max(1),
        (io.enters() - enters0) as f64 / requests.max(1) as f64,
        s3.connects()
    );
    std::process::exit(0);
}
