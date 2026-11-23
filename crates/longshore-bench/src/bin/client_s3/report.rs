// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The measured window of `client_s3 put|get|mix` and the JSON line it ends
//! with.

use crate::worker::Shared;
use crate::Args;
use longshore::client::Client;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Let connections settle, measure for `--secs`, print the result, and tell
/// the callers to stop.
pub fn measure(client: &Client, a: &Args, checksums: bool, s: &Shared) {
    // Let connections open and settle before the measured window.
    std::thread::sleep(Duration::from_secs(2));
    let cpu0 = process_cpu_secs();
    s.measuring.store(true, Ordering::Relaxed);
    let t0 = Instant::now();
    wait_out(client, a.secs, s, t0);
    s.measuring.store(false, Ordering::Relaxed);
    let wall = t0.elapsed().as_secs_f64();
    let cpu_s = process_cpu_secs() - cpu0;
    let gb = s.bytes.load(Ordering::Relaxed) as f64 / 1e9;
    let Args {
        cmd,
        threads,
        callers,
        conns,
        size,
        ..
    } = a;
    println!(
        "{{\"cmd\":\"client-{cmd}\",\"arch\":\"{}\",\"threads\":{threads},\"callers\":{callers},\"checksums\":{checksums},\"conns\":{conns},\"size\":{size},\"secs\":{wall:.2},\"requests\":{},\"failed\":{},\"puts\":{},\"gets\":{},\"wrong\":{},\"gb_per_s\":{:.3},\"process_cpu_cores\":{:.2},\"gb_per_cpu_s\":{:.2}}}",
        std::env::consts::ARCH,
        s.requests.load(Ordering::Relaxed),
        s.failed.load(Ordering::Relaxed),
        s.puts.load(Ordering::Relaxed),
        s.gets.load(Ordering::Relaxed),
        s.wrong.load(Ordering::Relaxed),
        gb / wall,
        cpu_s / wall,
        gb / cpu_s.max(1e-9),
    );
    s.stop.store(true, Ordering::Relaxed);
}

/// Sleep out the measured window. S3_REPORT_SECS=N prints a line every N
/// seconds of a long run: what the last interval moved, failures so far,
/// and the process's descriptors and resident memory, which should not grow.
fn wait_out(client: &Client, secs: u64, s: &Shared, t0: Instant) {
    let report = std::env::var("S3_REPORT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    match report.filter(|n| *n > 0) {
        None => std::thread::sleep(Duration::from_secs(secs)),
        Some(every) => {
            let mut last = 0u64;
            while t0.elapsed().as_secs() < secs {
                std::thread::sleep(Duration::from_secs(every.min(secs)));
                let now = s.bytes.load(Ordering::Relaxed);
                let (adopted, reload_failures) = client.credential_reloads();
                eprintln!(
                    "t={}s gb_per_s={:.2} failed={} puts={} gets={} wrong={} fds={} rss_mb={} reloads_adopted={adopted} reload_failures={reload_failures}",
                    t0.elapsed().as_secs(),
                    (now - last) as f64 / 1e9 / every as f64,
                    s.failed.load(Ordering::Relaxed),
                    s.puts.load(Ordering::Relaxed),
                    s.gets.load(Ordering::Relaxed),
                    s.wrong.load(Ordering::Relaxed),
                    std::fs::read_dir("/proc/self/fd").map(|d| d.count()).unwrap_or(0),
                    resident_mb(),
                );
                last = now;
            }
        }
    }
}

/// Resident memory of the process in MiB.
fn resident_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / (1 << 20))
}

/// User and system CPU time of the whole process, callers included.
fn process_cpu_secs() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = stat.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    // Fields 14 and 15 of the line, which are 11 and 12 after the name.
    (ticks(11) + ticks(12)) / 100.0
}
