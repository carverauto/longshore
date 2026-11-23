// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Request-rate benchmarks for operations where bytes per second is not the point.

use super::*;
use crate::die::OrDie;

const LISTED: u32 = 200;
const RANGE: u64 = 4096;

/// Create what the operation needs to exist: `conns` objects of `size`
/// bytes for `get-range`, 200 one-byte keys for `list-keys`, nothing otherwise.
fn seed(c: &mut Client, kind: &str, conns: u32, run: &str) {
    let seeded = match kind {
        "get-range" => conns,
        "list-keys" => LISTED,
        _ => 0,
    };
    let seed_len = if kind == "list-keys" { 1 } else { c.size };
    let mut made = 0;
    while made < seeded {
        let batch = conns.min(seeded - made);
        for i in 0..batch {
            let key = if kind == "list-keys" {
                format!("{run}/list/{:05}", made + i)
            } else {
                format!("{run}/{}", made + i)
            };
            c.s3.store(
                &mut c.io,
                u64::from(i),
                &key,
                BufId::new(c.first + i),
                seed_len,
                Checksum::None,
            )
            .unwrap_or_else(|e| die(format_args!("seed: {e:?}")));
        }
        for _ in 0..batch {
            if wait(&mut c.io, &mut c.s3, &mut c.out).outcome != Outcome::Stored {
                die("could not create the objects this benchmark reads");
            }
        }
        made += batch;
    }
}

/// The ring, the client, and what every request uses.
struct Client {
    io: UringIo,
    s3: S3,
    out: Completions,
    /// The first of the `conns` caller buffers; request `i` uses `first + i`.
    first: u32,
    size: u32,
}

/// What starts the next request on a connection. Keys are written into a
/// fixed buffer: nothing may allocate once the run starts.
struct Starter<'a> {
    kind: &'a str,
    prefix: &'a str,
    list_prefix: &'a str,
    seq: u64,
}

impl Starter<'_> {
    /// `prefix` followed by `n` in decimal, in `buf`; returns its length.
    fn key_for(&self, n: u64, buf: &mut [u8; 96]) -> usize {
        let p = self.prefix.as_bytes();
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
    }

    /// Start request `i` and note when it started in `start_ns`.
    fn start(&mut self, c: &mut Client, i: u32, start_ns: &mut [u64]) {
        let (client, ring, size) = (&mut c.s3, &mut c.io, c.size);
        let mut key = [0u8; 96];
        let buf = BufId::new(c.first + i);
        let r = match self.kind {
            "put-new" => {
                self.seq += 1;
                let n = self.key_for(self.seq, &mut key);
                client.store_no_clobber(
                    ring,
                    u64::from(i),
                    core::str::from_utf8(&key[..n]).or_die("UTF-8"),
                    buf,
                    size,
                    Checksum::None,
                )
            }
            "get-range" => {
                let n = self.key_for(u64::from(i), &mut key);
                client.fetch(
                    ring,
                    u64::from(i),
                    core::str::from_utf8(&key[..n]).or_die("UTF-8"),
                    Some((0, RANGE.min(u64::from(size)) - 1)),
                    buf,
                )
            }
            "erase" => {
                self.seq += 1;
                let n = self.key_for(1_000_000_000 + self.seq, &mut key);
                client.erase(
                    ring,
                    u64::from(i),
                    core::str::from_utf8(&key[..n]).or_die("UTF-8"),
                )
            }
            _ => client.list(ring, u64::from(i), self.list_prefix, None, 1000, buf),
        };
        if r.is_err() {
            std::process::exit(4);
        }
        start_ns[i as usize] = ring.now_ns();
    }
}

/// The measured window: restart each request as it finishes until
/// `deadline`, counting its latency into `hist`. Returns the requests,
/// attempts and bytes it saw.
fn measure(
    c: &mut Client,
    starter: &mut Starter,
    started: &mut [u64],
    deadline: u64,
    hist: &mut [u64; 32],
) -> (u64, u64, u64) {
    let (kind, size) = (starter.kind, c.size);
    sealed(|| {
        let (mut requests, mut attempts, mut bytes) = (0u64, 0u64, 0u64);
        while c.io.now_ns() < deadline {
            turn(&mut c.io, &mut c.s3, &mut c.out);
            while let Some(d) = c.s3.poll() {
                let i = d.token as usize;
                match (kind, d.outcome) {
                    ("put-new", Outcome::Stored) => bytes += u64::from(size),
                    ("get-range", Outcome::Fetched { len, .. }) => bytes += u64::from(len),
                    ("erase", Outcome::Erased) => {}
                    ("list-keys", Outcome::Listed { len }) => bytes += u64::from(len),
                    _ => std::process::exit(3),
                }
                let us = ((c.io.now_ns() - started[i]) / 1000).max(1);
                hist[(63 - us.leading_zeros() as usize).min(31)] += 1;
                requests += 1;
                attempts += u64::from(d.attempts);
                starter.start(c, i as u32, started);
            }
        }
        (requests, attempts, bytes)
    })
}

/// The upper edge of the bucket of `hist` that holds the given percentage
/// of `requests`.
fn percentile(hist: &[u64; 32], requests: u64, percent: u64) -> u64 {
    let want = (requests * percent).div_ceil(100);
    let mut seen = 0;
    for (k, n) in hist.iter().enumerate() {
        seen += n;
        if seen >= want.max(1) {
            return 1 << (k + 1);
        }
    }
    0
}

/// Request-rate benchmarks for the operations where bytes per second is
/// not the point: a conditional store of a new key (`put-new`), a ranged
/// fetch of the first 4 KiB (`get-range`), an erase (`erase`, of keys
/// that do not exist, so nothing has to be created first), and a
/// listing of 200 keys (`list-keys`). Reports requests per second, client CPU
/// per request, and latency percentiles from a log-scale histogram.
pub(super) fn ops(kind: &str, conns: u32, size: u32, secs: u64, cpu: Option<usize>) {
    pin(cpu);
    // A listing of 200 keys is about 60 kB of XML.
    let buffer = if kind == "list-keys" {
        size.max(256 * 1024)
    } else {
        size.max(RANGE as u32)
    };
    let (io, s3, first) = setup(conns, conns, buffer, conns);
    let mut c = Client {
        io,
        s3,
        out: Completions::new(64),
        first,
        size,
    };
    let run = format!("bench/ops-{}", std::process::id());
    seed(&mut c, kind, conns, &run);
    settle(&mut c.io, &mut c.s3, &mut c.out);
    let list_prefix = format!("{run}/list/");
    let prefix = format!("{run}/");
    let mut starter = Starter {
        kind,
        prefix: &prefix,
        list_prefix: &list_prefix,
        seq: 0,
    };
    let mut started = vec![0u64; conns as usize];
    for i in 0..conns {
        starter.start(&mut c, i, &mut started);
    }
    let (t0, cpu0, enters0) = (c.io.now_ns(), thread_cpu_secs(), c.io.enters());
    let deadline = t0 + secs * 1000 * MS;
    // Latency histogram: bucket k holds requests that took [2^k, 2^(k+1)) microseconds.
    let mut hist = [0u64; 32];
    let (requests, attempts, bytes) =
        measure(&mut c, &mut starter, &mut started, deadline, &mut hist);
    let wall = (c.io.now_ns() - t0) as f64 / 1e9;
    let cpu_s = thread_cpu_secs() - cpu0;
    println!(
        "{{\"cmd\":\"{kind}\",\"arch\":\"{}\",\"conns\":{conns},\"size\":{size},\"secs\":{wall:.2},\"requests\":{requests},\"attempts\":{attempts},\"requests_per_s\":{:.0},\"cpu_us_per_request\":{:.1},\"thread_cpu\":{:.3},\"p50_us_below\":{},\"p99_us_below\":{},\"bytes_per_request\":{},\"enters_per_request\":{:.1},\"connects\":{}}}",
        std::env::consts::ARCH,
        requests as f64 / wall,
        cpu_s * 1e6 / requests.max(1) as f64,
        cpu_s / wall,
        percentile(&hist, requests, 50),
        percentile(&hist, requests, 99),
        bytes / requests.max(1),
        (c.io.enters() - enters0) as f64 / requests.max(1) as f64,
        c.s3.connects()
    );
    std::process::exit(0);
}
