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
#[path = "client_s3/worker.rs"]
mod worker;

#[cfg(target_os = "linux")]
#[path = "client_s3/report.rs"]
mod report;

/// Print `client_s3: <msg>` and exit with status 2.
#[cfg(target_os = "linux")]
fn die(msg: &str) -> ! {
    eprintln!("client_s3: {msg}");
    std::process::exit(2);
}

/// An environment variable the run cannot do without.
#[cfg(target_os = "linux")]
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| die(&format!("{name} is not set")))
}

/// What the command line asked for.
#[cfg(target_os = "linux")]
struct Args {
    cmd: String,
    put: bool,
    mix: bool,
    puts_per_sec: u64,
    parts: u32,
    objects: u32,
    threads: u32,
    conns: u32,
    callers: u32,
    size: u32,
    secs: u64,
    cpu: Option<usize>,
    cpu_step: usize,
}

#[cfg(target_os = "linux")]
impl Args {
    fn parse() -> Self {
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
        let mut a = Self {
            cmd,
            put,
            mix,
            puts_per_sec: 100,
            parts: 4,
            objects: 4,
            threads: 1,
            conns: 64,
            callers: 0,
            size: 8 << 20,
            secs: 10,
            cpu: None,
            cpu_step: 1,
        };
        while let Some(k) = args.next() {
            let v: u64 = args
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| die(&format!("bad value for {k}")));
            match k.as_str() {
                "--threads" => a.threads = v as u32,
                "--conns" => a.conns = v as u32,
                "--callers" => a.callers = v as u32,
                "--size" => a.size = v as u32,
                "--secs" => a.secs = v,
                "--cpu" => a.cpu = Some(v as usize),
                "--cpu-step" => a.cpu_step = v as usize,
                "--puts-per-sec" => a.puts_per_sec = v,
                "--parts" => a.parts = v as u32,
                "--objects" => a.objects = v as u32,
                _ => die(&format!("unknown flag {k}")),
            }
        }
        // One caller thread per I/O thread unless told otherwise.
        if a.callers == 0 {
            a.callers = a.threads;
        }
        a
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let a = Args::parse();
    // S3_CLIENT_CHECKSUM=0 sends bodies without a checksum, as `s3_uring put` does.
    let checksums = !std::env::var("S3_CLIENT_CHECKSUM").is_ok_and(|v| v == "0");
    let client = build(&a, checksums);
    if a.cmd == "multipart" {
        if a.parts == 0 || a.parts > a.threads * a.conns {
            die("--parts must be 1 to the number of buffers (threads x conns)");
        }
        multipart::run(&client, a.size as usize, a.parts, a.objects);
        drop(client);
        std::process::exit(0);
    }
    let shared = worker::Shared::new(a.callers);
    let workers = worker::spawn(&client, &a, &shared);
    let _: std::sync::BarrierWaitResult = shared.ready.wait();
    report::measure(&client, &a, checksums, &shared);
    drop(workers);
    std::process::exit(0);
}

/// The client the store's environment variables describe.
#[cfg(target_os = "linux")]
fn build(a: &Args, checksums: bool) -> longshore::client::Client {
    use longshore::client::Builder;
    use longshore::store::creds::Credentials;
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
    let b = Builder::new(&host, &env("S3_REGION"), credentials)
        .bucket_in_path(path_style.then(|| env("S3_BUCKET")))
        // S3_CREDENTIALS_FILE: an absolute path the client rereads its keys from.
        .credentials_file(std::env::var("S3_CREDENTIALS_FILE").ok().map(Into::into))
        .checksums(checksums)
        .tls(tls)
        .port(if tls { 443 } else { 80 })
        .threads(a.threads)
        .pin(a.cpu.map(|c| (c, a.cpu_step)))
        .connections(a.conns)
        .buffers(a.conns)
        .operations(a.conns)
        .buffer_size(a.size)
        .prefer_large_segments(
            std::env::var("S3_PREFER_LARGE_SEGMENTS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 0),
        );
    b.build().unwrap_or_else(|e| die(&e.to_string()))
}
