// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Throughput of the shareable client (`longshore::client`) against a store.
//!
//!   client_s3 put|get|mix [--threads N] [--conns N] [--callers N]
//!             [--size BYTES] [--secs S] [--cpu N] [--cpu-step N]
//!             [--puts-per-sec N]
//!
//! `mix` is for long runs: it fetches as fast as it can and stores at most
//! `--puts-per-sec` objects a second (default 100), since a store is the
//! request a store charges most for. Every object holds a pattern that
//! names it, and every fetch checks bytes of that pattern.
//!
//!   client_s3 multipart [--parts N] [--objects N] [--size BYTES] [--threads N] [--conns N]
//!
//! uploads objects of `--parts` parts of `--size` bytes each through the
//! client's multipart calls, with an object's parts in flight together,
//! and checks every part with a ranged fetch.
//!
//! The store comes from the same environment variables as `s3_uring`:
//! S3_ENDPOINT, S3_REGION, S3_BUCKET, S3_ACCESS_KEY, S3_SECRET_KEY,
//! S3_SESSION_TOKEN, S3_TLS, S3_PATH_STYLE, S3_PREFER_LARGE_SEGMENTS, and
//! S3_CREDENTIALS_FILE. S3_CLIENT_CHECKSUM=0 turns off the CRC-64 the client
//! otherwise sends with every body.
//!
//! `--threads` is the client's I/O threads and `--conns` the connections of
//! each. `--callers` threads share the client, each keeping a part of
//! the operations in flight through futures. Prints one JSON line.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("client_s3 needs Linux");
}

#[cfg(target_os = "linux")]
#[path = "client_s3/multipart.rs"]
mod multipart;

#[cfg(target_os = "linux")]
fn main() {
    use longshore::client::{Buffer, Builder, Client, Error, Pending};
    use longshore::store::creds::Credentials;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::{Duration, Instant};

    fn die(msg: &str) -> ! {
        eprintln!("client_s3: {msg}");
        std::process::exit(2);
    }
    let env =
        |name: &str| std::env::var(name).unwrap_or_else(|_| die(&format!("{name} is not set")));

    let mut args = std::env::args().skip(1);
    let cmd = args
        .next()
        .unwrap_or_else(|| die("usage: client_s3 put|get|mix|multipart [flags]"));
    let (put, mix) = match cmd.as_str() {
        "put" => (true, false),
        "get" => (false, false),
        "mix" => (false, true),
        "multipart" => (true, false),
        other => die(&format!("unknown command {other}")),
    };
    let mut puts_per_sec = 100u64;
    let (mut parts, mut objects) = (4u32, 4u32);
    let (mut threads, mut conns, mut callers, mut size, mut secs) =
        (1u32, 64u32, 0u32, 8u32 << 20, 10u64);
    let (mut cpu, mut cpu_step) = (None, 1usize);
    while let Some(k) = args.next() {
        let v: u64 = args
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| die(&format!("bad value for {k}")));
        match k.as_str() {
            "--threads" => threads = v as u32,
            "--conns" => conns = v as u32,
            "--callers" => callers = v as u32,
            "--size" => size = v as u32,
            "--secs" => secs = v,
            "--cpu" => cpu = Some(v as usize),
            "--cpu-step" => cpu_step = v as usize,
            "--puts-per-sec" => puts_per_sec = v,
            "--parts" => parts = v as u32,
            "--objects" => objects = v as u32,
            _ => die(&format!("unknown flag {k}")),
        }
    }
    // One caller thread per I/O thread unless told otherwise.
    let callers = if callers == 0 { threads } else { callers };
    let tls = std::env::var("S3_TLS").is_ok_and(|v| v == "1");
    let credentials = match std::env::var("S3_SESSION_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
    {
        Some(token) => {
            Credentials::with_session(&env("S3_ACCESS_KEY"), &env("S3_SECRET_KEY"), token.trim())
        }
        None => Credentials::new(&env("S3_ACCESS_KEY"), &env("S3_SECRET_KEY")),
    }
    .unwrap_or_else(|| die("the credentials are empty, too long or not printable ASCII"));
    // S3_PATH_STYLE=1 addresses the bucket in the path.
    let path_style = std::env::var("S3_PATH_STYLE").is_ok_and(|v| v == "1");
    let host = if path_style {
        env("S3_ENDPOINT")
    } else {
        format!("{}.{}", env("S3_BUCKET"), env("S3_ENDPOINT"))
    };
    let mut b = Builder::new(&host, &env("S3_REGION"), credentials);
    b.bucket_in_path = path_style.then(|| env("S3_BUCKET"));
    // S3_CREDENTIALS_FILE: an absolute path the client rereads its keys from.
    b.credentials_file = std::env::var("S3_CREDENTIALS_FILE").ok().map(Into::into);
    // S3_CLIENT_CHECKSUM=0 sends bodies without a checksum, as `s3_uring put` does.
    let checksums = !std::env::var("S3_CLIENT_CHECKSUM").is_ok_and(|v| v == "0");
    b.checksums = checksums;
    b.tls = tls;
    b.port = if tls { 443 } else { 80 };
    b.threads = threads;
    b.pin = cpu.map(|c| (c, cpu_step));
    b.connections = conns;
    b.buffers = conns;
    b.operations = conns;
    b.buffer_size = size;
    b.prefer_large_segments = std::env::var("S3_PREFER_LARGE_SEGMENTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let client = b.build().unwrap_or_else(|e| die(&e.to_string()));
    if cmd == "multipart" {
        if parts == 0 || parts > threads * conns {
            die("--parts must be 1 to the number of buffers (threads x conns)");
        }
        multipart::run(&client, size as usize, parts, objects);
        drop(client);
        std::process::exit(0);
    }

    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    /// One operation in flight, of either kind.
    enum Op {
        Put(Pending<(Result<(), Error>, Buffer)>),
        Get(Pending<(Result<Option<usize>, Error>, Buffer)>),
    }

    impl Op {
        fn begin(client: &Client, put: bool, key: &str, buffer: Buffer, size: usize) -> Op {
            if put {
                Op::Put(client.store(key, buffer, size))
            } else {
                Op::Get(client.fetch(key, buffer))
            }
        }

        /// Whether it ended well, and its buffer, once it has ended.
        fn poll(&mut self, cx: &mut Context<'_>, size: usize) -> Poll<(bool, Buffer)> {
            match self {
                Op::Put(p) => Pin::new(p).poll(cx).map(|(r, b)| (r == Ok(()), b)),
                Op::Get(p) => Pin::new(p).poll(cx).map(|(r, b)| (r == Ok(Some(size)), b)),
            }
        }

        fn is_put(&self) -> bool {
            matches!(self, Op::Put(_))
        }
    }

    /// The byte at `at` of the object a caller keeps under its key `i`.
    fn pattern(c: u32, i: usize, at: usize) -> u8 {
        (at as u32).wrapping_mul(2654435761).to_le_bytes()[2] ^ (c as usize * 131 + i) as u8
    }
    /// Places in an object that a fetch checks: its ends and some between.
    fn sampled(size: usize) -> impl Iterator<Item = usize> {
        (0..64).map(move |k| k * (size - 1) / 63)
    }

    // Each caller holds a share of the buffers, and the shares sum to all
    // of them, so no caller waits for one that another holds for good.
    let total = (threads * conns) as usize;
    let share = |c: usize| total / callers as usize + usize::from(c < total % callers as usize);
    let (bytes, requests) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    // S3_TOLERATE_ERRORS=1 counts operations that fail and carries on, for
    // runs that break connections on purpose. Otherwise the first one ends
    // the run.
    let tolerate = std::env::var("S3_TOLERATE_ERRORS").is_ok_and(|v| v == "1");
    let failed = Arc::new(AtomicU64::new(0));
    // For `mix`: stores begun, fetches ended, and fetches whose bytes were
    // not the object's.
    let (puts, gets, wrong) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    );
    let started = Instant::now();
    let (measuring, stop) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let ready = Arc::new(Barrier::new(callers as usize + 1));
    let pid = std::process::id();
    let workers: Vec<_> = (0..callers)
        .map(|c| {
            let (client, bytes, requests) = (client.clone(), bytes.clone(), requests.clone());
            let failed = failed.clone();
            let (puts, gets, wrong) = (puts.clone(), gets.clone(), wrong.clone());
            let (measuring, stop, ready) = (measuring.clone(), stop.clone(), ready.clone());
            let mine = share(c as usize);
            std::thread::spawn(move || {
                let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
                let mut cx = Context::from_waker(&waker);
                let keys: Vec<String> = (0..mine)
                    .map(|i| format!("bench/{pid}-c{c}-{i}.bin"))
                    .collect();
                let fill = |i: usize, buffer: &mut Buffer| {
                    for (at, byte) in buffer.as_mut_slice().iter_mut().enumerate() {
                        *byte = pattern(c, i, at);
                    }
                };
                // In `mix`, the next operation on a key is a store while
                // the run is behind its rate of stores, and a fetch if not.
                let next = |i: usize, mut buffer: Buffer| {
                    let due = started.elapsed().as_millis() as u64 * puts_per_sec / 1000;
                    let store = put || (mix && puts.fetch_add(1, Ordering::Relaxed) < due);
                    if mix && !store {
                        let _: u64 = puts.fetch_sub(1, Ordering::Relaxed);
                    }
                    if mix && store {
                        fill(i, &mut buffer);
                    }
                    Op::begin(&client, store, &keys[i], buffer, size as usize)
                };
                // For GET the objects must exist first.
                if !put {
                    for (i, key) in keys.iter().enumerate() {
                        let mut body = client.buffer_blocking();
                        if mix {
                            fill(i, &mut body);
                        }
                        let (stored, _b) = client.store(key, body, size as usize).wait();
                        if stored.is_err() {
                            die("could not create an object to fetch");
                        }
                    }
                }
                let _: std::sync::BarrierWaitResult = ready.wait();
                let mut ops: Vec<(usize, Op)> = (0..keys.len())
                    .map(|i| (i, next(i, client.buffer_blocking())))
                    .collect();
                while !stop.load(Ordering::Relaxed) {
                    let mut progressed = false;
                    for (i, op) in ops.iter_mut() {
                        if let Poll::Ready((ok, buffer)) = op.poll(&mut cx, size as usize) {
                            if !ok && !tolerate {
                                eprintln!("client_s3: an operation failed");
                                std::process::exit(3);
                            }
                            if !ok {
                                let _: u64 = failed.fetch_add(1, Ordering::Relaxed);
                            } else if measuring.load(Ordering::Relaxed) {
                                let _: u64 = bytes.fetch_add(size as u64, Ordering::Relaxed);
                                let _: u64 = requests.fetch_add(1, Ordering::Relaxed);
                            }
                            if mix && ok && !op.is_put() {
                                let _: u64 = gets.fetch_add(1, Ordering::Relaxed);
                                let got = buffer.as_slice();
                                if sampled(size as usize).any(|at| got[at] != pattern(c, *i, at)) {
                                    let _: u64 = wrong.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            *op = next(*i, buffer);
                            progressed = true;
                        }
                    }
                    if !progressed {
                        std::thread::park_timeout(Duration::from_millis(50));
                    }
                }
            })
        })
        .collect();
    let _: std::sync::BarrierWaitResult = ready.wait();
    // Let connections open and settle before the measured window.
    std::thread::sleep(Duration::from_secs(2));
    let cpu0 = process_cpu_secs();
    measuring.store(true, Ordering::Relaxed);
    let t0 = Instant::now();
    // S3_REPORT_SECS=N prints a line every N seconds of a long run: what the
    // last interval moved, failures so far, and the process's descriptors
    // and resident memory, which should not grow.
    let report = std::env::var("S3_REPORT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    match report.filter(|n| *n > 0) {
        None => std::thread::sleep(Duration::from_secs(secs)),
        Some(every) => {
            let mut last = 0u64;
            while t0.elapsed().as_secs() < secs {
                std::thread::sleep(Duration::from_secs(every.min(secs)));
                let now = bytes.load(Ordering::Relaxed);
                let (adopted, reload_failures) = client.credential_reloads();
                eprintln!(
                    "t={}s gb_per_s={:.2} failed={} puts={} gets={} wrong={} fds={} rss_mb={} reloads_adopted={adopted} reload_failures={reload_failures}",
                    t0.elapsed().as_secs(),
                    (now - last) as f64 / 1e9 / every as f64,
                    failed.load(Ordering::Relaxed),
                    puts.load(Ordering::Relaxed),
                    gets.load(Ordering::Relaxed),
                    wrong.load(Ordering::Relaxed),
                    std::fs::read_dir("/proc/self/fd").map(|d| d.count()).unwrap_or(0),
                    resident_mb(),
                );
                last = now;
            }
        }
    }
    measuring.store(false, Ordering::Relaxed);
    let wall = t0.elapsed().as_secs_f64();
    let cpu_s = process_cpu_secs() - cpu0;
    let gb = bytes.load(Ordering::Relaxed) as f64 / 1e9;
    println!(
        "{{\"cmd\":\"client-{cmd}\",\"arch\":\"{}\",\"threads\":{threads},\"callers\":{callers},\"checksums\":{checksums},\"conns\":{conns},\"size\":{size},\"secs\":{wall:.2},\"requests\":{},\"failed\":{},\"puts\":{},\"gets\":{},\"wrong\":{},\"gb_per_s\":{:.3},\"process_cpu_cores\":{:.2},\"gb_per_cpu_s\":{:.2}}}",
        std::env::consts::ARCH,
        requests.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
        puts.load(Ordering::Relaxed),
        gets.load(Ordering::Relaxed),
        wrong.load(Ordering::Relaxed),
        gb / wall,
        cpu_s / wall,
        gb / cpu_s.max(1e-9),
    );
    stop.store(true, Ordering::Relaxed);
    drop(workers);
    std::process::exit(0);

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
}
