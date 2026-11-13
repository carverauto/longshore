// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The Linux implementation of this program.

mod checks;
use checks::*;
mod multipart;
use multipart::*;
mod objects;
use objects::*;
mod ops;
use ops::*;
mod throughput;
use throughput::*;

use longshore::dns::codec::parse_resolv_conf;
use longshore::dns::{Family, Resolver};
use longshore::http::client;
use longshore::io::alloc_guard::sealed;
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::store::s3::{Checksum, Config, Done, Endpoint, Outcome, S3};
use std::net::SocketAddr;

const HEARTBEAT: u64 = 1 << 50;
const MS: u64 = 1_000_000;

/// A checksum to compare two buffers by without copying either.
fn checksum_of(bytes: &[u8]) -> u64 {
    longshore::checksum::crc64_nvme(bytes)
}

fn die(msg: &str) -> ! {
    eprintln!("s3_uring: {msg}");
    std::process::exit(1)
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| die(&format!("{name} is not set")))
}

extern "C" {
    fn sched_setaffinity(pid: i32, len: usize, mask: *const u64) -> i32;
    fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
}

fn thread_cpu_secs() -> f64 {
    let mut ts = [0i64; 2];
    unsafe { clock_gettime(3, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

/// Large buffers: `2 * conns` for TLS records (used only with S3_TLS=1),
/// then `user` caller buffers. Small buffers: `conns` HTTP heads, 3 for DNS.
/// Sockets: `conns` for HTTP, 2 for DNS.
fn setup(conns: u32, user: u32, size: u32, requests: u32) -> (UringIo, S3, u32) {
    let host = format!("{}.{}", env("S3_BUCKET"), env("S3_ENDPOINT"));
    let conf = std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_else(|e| die(&format!("/etc/resolv.conf: {e}")));
    let servers: Vec<SocketAddr> = parse_resolv_conf(&conf)
        .into_iter()
        .take(2)
        .map(|ip| SocketAddr::new(ip, 53))
        .collect();
    if servers.is_empty() {
        die("no nameserver in /etc/resolv.conf");
    }
    // S3_FAMILY: 4 (the default), 6, prefer6 or prefer4.
    let family = match std::env::var("S3_FAMILY").as_deref() {
        Err(_) | Ok("4") => Family::V4,
        Ok("6") => Family::V6,
        Ok("prefer6") => Family::PreferV6,
        Ok("prefer4") => Family::PreferV4,
        Ok(other) => die(&format!(
            "S3_FAMILY={other}: expected 4, 6, prefer6 or prefer4"
        )),
    };
    let tls = std::env::var("S3_TLS").is_ok_and(|v| v == "1");
    // S3_VENDOR=gcs: Google Cloud Storage's XML API with S3_BEARER_TOKEN
    // in place of the access and secret keys.
    let gcs = std::env::var("S3_VENDOR").is_ok_and(|v| v == "gcs");
    // TLS record buffers must hold at least two records.
    // Also never smaller than a request or response head.
    let buffer_size = size.max(if tls { 256 * 1024 } else { 8192 });
    let limits = Limits {
        sockets_max: conns + 2,
        buffers: 2 * conns + user,
        buffer_size,
        small_buffers: conns + 3,
        small_buffer_size: 8192,
        ops_max: conns * 3 + 4 + 1,
    };
    let mut io = UringIo::new(limits).unwrap_or_else(|e| die(&e.to_string()));
    let resolver = Resolver::new(longshore::dns::Config {
        servers,
        hosts: vec![host.clone()],
        family,
        refresh_ns: 30_000 * MS,
        timeout_ns: 2_000 * MS,
        socks: [Sock(conns), Sock(conns + 1)],
        rx_bufs: [limits.small(conns), limits.small(conns + 1)],
        tx_buf: limits.small(conns + 2),
        token_base: 3 << 40,
    });
    let http = client::Config {
        connections_max: conns,
        first_sock: 0,
        first_buf: limits.small(0).0,
        head_max: 8192,
        buffer_size,
        max_age_ns: 300_000 * MS,
        max_requests: 100_000,
        request_timeout_ns: 60_000 * MS,
        zero_copy_min: 64 * 1024,
        token_base: 1 << 40,
        tls_batch: std::env::var("S3_TLS_BATCH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1 << 20),
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    };
    let mut s3 = S3::new(Config {
        vendor: if gcs {
            longshore::store::s3::Vendor::Gcs
        } else {
            longshore::store::s3::Vendor::S3
        },
        host,
        port: if tls { 443 } else { 80 },
        region: if gcs { String::new() } else { env("S3_REGION") },
        credentials: if gcs {
            longshore::store::creds::Credentials::bearer(env("S3_BEARER_TOKEN").trim())
                .unwrap_or_else(|| die("S3_BEARER_TOKEN is empty, too long or not printable ASCII"))
        } else {
            // S3_SESSION_TOKEN: the token that goes with temporary keys.
            match std::env::var("S3_SESSION_TOKEN").ok().filter(|t| !t.is_empty()) {
                Some(token) => longshore::store::creds::Credentials::with_session(
                    &env("S3_ACCESS_KEY"),
                    &env("S3_SECRET_KEY"),
                    token.trim(),
                ),
                None => longshore::store::creds::Credentials::new(&env("S3_ACCESS_KEY"), &env("S3_SECRET_KEY")),
            }
            .unwrap_or_else(|| {
                die("S3_ACCESS_KEY, S3_SECRET_KEY or S3_SESSION_TOKEN is empty, too long or not printable ASCII")
            })
        },
        credentials_file: None,
        requests_max: requests,
        attempts_max: 4,
        backoff_ns: 200 * MS,
        backoff_max_ns: 5_000 * MS,
        // Linode returns no checksum on fetch; set S3_REQUIRE_CHECKSUMS=1 for stores that do.
        require_checksums: std::env::var("S3_REQUIRE_CHECKSUMS").is_ok_and(|v| v == "1"),
        http,
        tls: tls.then(|| {
            (
                if std::env::var("S3_TLS_AES128").is_ok_and(|v| v == "1") {
                    longshore::tls::handshake::ClientSetup::with_public_roots_aes128()
                } else {
                    longshore::tls::handshake::ClientSetup::with_public_roots()
                },
                0,
            )
        }),
        endpoint: Endpoint::Resolved(resolver),
    });
    s3.start(&mut io);
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 50 * MS })
        .unwrap();
    (io, s3, 2 * conns)
}

/// One loop turn: wait for completions, route them, run the periodic tick.
fn turn(io: &mut UringIo, s3: &mut S3, out: &mut Completions) {
    io.tick(true, out).expect("tick");
    while let Some(c) = out.pop() {
        if c.token.0 == HEARTBEAT {
            s3.tick(io);
            io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 50 * MS })
                .unwrap();
        } else {
            s3.on_completion(io, c);
        }
    }
}

fn wait(io: &mut UringIo, s3: &mut S3, out: &mut Completions) -> Done {
    loop {
        if let Some(d) = s3.poll() {
            return d;
        }
        turn(io, s3, out);
    }
}

pub fn main() {
    let mut args = std::env::args().skip(1);
    let first = std::env::args().nth(1);
    if matches!(first.as_deref(), Some("list" | "clean")) {
        let prefix = std::env::args()
            .nth(2)
            .unwrap_or_else(|| die("usage: s3_uring list|clean PREFIX"));
        return sweep(&prefix, first.as_deref() == Some("clean"));
    }
    let cmd = args.next().unwrap_or_else(|| die("usage: s3_uring selftest | badcrc | put | put-crc | get | put-new | get-range | erase | list-keys [--conns N] [--size BYTES] [--secs S] [--cpu N]"));
    let (mut conns, mut size, mut secs, mut cpu) = (4u32, 8u32 << 20, 10u64, None);
    while let Some(k) = args.next() {
        let v: u64 = args
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| die(&format!("bad value for {k}")));
        match k.as_str() {
            "--conns" => conns = v as u32,
            "--size" => size = v as u32,
            "--secs" => secs = v,
            "--cpu" => cpu = Some(v as usize),
            _ => die(&format!("unknown flag {k}")),
        }
    }
    match cmd.as_str() {
        "selftest" => selftest(size),
        "badcrc" => bad_checksum(size),
        "multipart" => multipart(size),
        "objects" => objects(size),
        "put" => bench(true, conns, size, secs, cpu, Checksum::None),
        "put-crc" => bench(true, conns, size, secs, cpu, Checksum::Compute),
        "get" => bench(false, conns, size, secs, cpu, Checksum::None),
        "put-new" | "get-range" | "erase" | "list-keys" => ops(&cmd, conns, size, secs, cpu),
        other => die(&format!("unknown command {other}")),
    }
}
