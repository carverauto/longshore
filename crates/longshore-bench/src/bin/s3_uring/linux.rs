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

use crate::die::{die, OrDie};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| die(format_args!("{name} is not set")))
}

extern "C" {
    fn sched_setaffinity(pid: i32, len: usize, mask: *const u64) -> i32;
    fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
}

fn thread_cpu_secs() -> f64 {
    let mut ts = [0i64; 2];
    let _: i32 = unsafe { clock_gettime(3, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

/// The host to connect to, and the bucket when it goes in the path.
/// S3_PATH_STYLE=1 addresses the bucket in the path, as self-hosted
/// stores commonly expect: requests go to /bucket/key on the endpoint.
fn host() -> (String, Option<String>) {
    let bucket_in_path = std::env::var("S3_PATH_STYLE")
        .is_ok_and(|v| v == "1")
        .then(|| env("S3_BUCKET"));
    let host = match &bucket_in_path {
        Some(_) => env("S3_ENDPOINT"),
        None => format!("{}.{}", env("S3_BUCKET"), env("S3_ENDPOINT")),
    };
    (host, bucket_in_path)
}

/// The first two name servers in /etc/resolv.conf.
fn name_servers() -> Vec<SocketAddr> {
    let conf = std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_else(|e| die(format_args!("/etc/resolv.conf: {e}")));
    let servers: Vec<SocketAddr> = parse_resolv_conf(&conf)
        .into_iter()
        .take(2)
        .map(|ip| SocketAddr::new(ip, 53))
        .collect();
    if servers.is_empty() {
        die("no nameserver in /etc/resolv.conf");
    }
    servers
}

/// S3_FAMILY: 4 (the default), 6, prefer6 or prefer4.
fn family() -> Family {
    match std::env::var("S3_FAMILY").as_deref() {
        Err(_) | Ok("4") => Family::V4,
        Ok("6") => Family::V6,
        Ok("prefer6") => Family::PreferV6,
        Ok("prefer4") => Family::PreferV4,
        Ok(other) => die(format_args!(
            "S3_FAMILY={other}: expected 4, 6, prefer6 or prefer4"
        )),
    }
}

/// S3_PREFER_LARGE_SEGMENTS=N: measure each connection's segment size and
/// keep connections on the addresses with large segments, using at least
/// N addresses. Unset or 0 turns it off.
fn prefer_large_segments() -> Option<usize> {
    std::env::var("S3_PREFER_LARGE_SEGMENTS")
        .ok()
        .map(|v| {
            v.parse::<usize>()
                .unwrap_or_else(|_| die("S3_PREFER_LARGE_SEGMENTS: expected a number of addresses"))
        })
        .filter(|n| *n > 0)
}

/// The resolver for `host`, on the two sockets after the `conns` HTTP ones
/// and the small buffers after the `conns` HTTP heads.
fn resolver(
    (host, servers, family): (&str, Vec<SocketAddr>, Family),
    conns: u32,
    limits: &Limits,
    prefer_large_segments: Option<usize>,
) -> Resolver {
    Resolver::new(longshore::dns::Config {
        servers,
        hosts: vec![host.to_string()],
        family,
        prefer_large_segments,
        refresh_ns: 30_000 * MS,
        timeout_ns: 2_000 * MS,
        socks: [Sock::new(conns), Sock::new(conns + 1)],
        rx_bufs: [limits.small(conns), limits.small(conns + 1)],
        tx_buf: limits.small(conns + 2),
        token_base: 3 << 40,
    })
    .unwrap_or_else(|e| die(e))
}

/// The HTTP client's configuration: `conns` connections on the first
/// sockets, their heads in the first small buffers.
fn http(conns: u32, limits: &Limits, buffer_size: u32) -> client::ClientConfig {
    client::ClientConfig::builder(conns, 0, limits.small(0).get(), buffer_size, 1 << 40)
        .head_max(8192)
        .max_age_ns(300_000 * MS)
        .max_requests(100_000)
        .request_timeout_ns(60_000 * MS)
        .zero_copy_min(64 * 1024)
        .tls_batch(
            std::env::var("S3_TLS_BATCH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3 << 20),
        )
        .build()
        .or_die("the HTTP client configuration")
}

/// S3_BEARER_TOKEN on Google Cloud Storage; otherwise S3_ACCESS_KEY and
/// S3_SECRET_KEY, with S3_SESSION_TOKEN when the keys are temporary.
fn credentials(gcs: bool) -> longshore::store::creds::Credentials {
    if gcs {
        return longshore::store::creds::Credentials::bearer(env("S3_BEARER_TOKEN").trim())
            .unwrap_or_else(|| die("S3_BEARER_TOKEN is empty, too long or not printable ASCII"));
    }
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
}

/// The TLS setup with public roots; S3_TLS_AES128=1 offers only AES-128.
fn tls_setup() -> (longshore::tls::handshake::ClientSetup, u32) {
    (
        if std::env::var("S3_TLS_AES128").is_ok_and(|v| v == "1") {
            longshore::tls::handshake::ClientSetup::with_public_roots_aes128()
        } else {
            longshore::tls::handshake::ClientSetup::with_public_roots()
        },
        0,
    )
}

/// Large buffers: `2 * conns` for TLS records (used only with S3_TLS=1),
/// then `user` caller buffers. Small buffers: `conns` HTTP heads, 3 for DNS.
/// Sockets: `conns` for HTTP, 2 for DNS.
fn setup(conns: u32, user: u32, size: u32, requests: u32) -> (UringIo, S3, u32) {
    let (host, bucket_in_path) = host();
    let servers = name_servers();
    let family = family();
    let tls = std::env::var("S3_TLS").is_ok_and(|v| v == "1");
    // S3_VENDOR=gcs: Google Cloud Storage's XML API with S3_BEARER_TOKEN
    // in place of the access and secret keys.
    let gcs = std::env::var("S3_VENDOR").is_ok_and(|v| v == "gcs");
    // TLS record buffers must hold at least two records.
    // Also never smaller than a request or response head.
    let buffer_size = size.max(if tls { 256 * 1024 } else { 8192 });
    let limits = Limits::BASE
        .with_sockets_max(conns + 2)
        .with_buffers(2 * conns + user)
        .with_buffer_size(buffer_size)
        .with_small_buffers(conns + 3)
        .with_small_buffer_size(8192)
        .with_ops_max(conns * 3 + 4 + 1);
    let mut io = UringIo::new(limits).unwrap_or_else(|e| die(e));
    let prefer_large_segments = prefer_large_segments();
    io.measure_segment_sizes(prefer_large_segments.is_some());
    let resolver = resolver(
        (&host, servers, family),
        conns,
        &limits,
        prefer_large_segments,
    );
    let mut s3 = S3::new(Config {
        vendor: if gcs {
            longshore::store::s3::Vendor::Gcs
        } else {
            longshore::store::s3::Vendor::S3
        },
        host,
        bucket_in_path,
        port: if tls { 443 } else { 80 },
        region: if gcs { String::new() } else { env("S3_REGION") },
        credentials: credentials(gcs),
        credentials_file: None,
        requests_max: requests,
        attempts_max: 4,
        backoff_ns: 200 * MS,
        backoff_max_ns: 5_000 * MS,
        // Linode returns no checksum on fetch; set S3_REQUIRE_CHECKSUMS=1 for stores that do.
        require_checksums: std::env::var("S3_REQUIRE_CHECKSUMS").is_ok_and(|v| v == "1"),
        http: http(conns, &limits, buffer_size),
        tls: tls.then(tls_setup),
        endpoint: Endpoint::Resolved(resolver),
    });
    s3.start(&mut io);
    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 50 * MS })
        .or_die("submit");
    (io, s3, 2 * conns)
}

/// One loop turn: wait for completions, route them, run the periodic tick.
fn turn(io: &mut UringIo, s3: &mut S3, out: &mut Completions) {
    io.tick(true, out).or_die("tick");
    while let Some(c) = out.pop() {
        if c.token().get() == HEARTBEAT {
            s3.tick(io);
            io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 50 * MS })
                .or_die("submit");
        } else {
            let _: bool = s3.on_completion(io, c);
        }
    }
}

/// Turn the loop until the client is ready to start requests.
fn settle(io: &mut UringIo, s3: &mut S3, out: &mut Completions) {
    while !s3.ready() {
        turn(io, s3, out);
    }
}

/// Pin the calling thread to `cpu`, if one is given.
fn pin(cpu: Option<usize>) {
    if let Some(cpu) = cpu {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        let _: i32 = unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
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
    let cmd = args.next().unwrap_or_else(|| die("usage: s3_uring selftest | badcrc | put | put-crc | get | put-new | get-range | erase | list-keys [--conns N] [--size BYTES] [--secs S] [--cpu N] [--threads N] [--cpu-step N]"));
    let (mut conns, mut size, mut secs, mut cpu) = (4u32, 8u32 << 20, 10u64, None);
    // --threads N runs N independent clients, each with --conns connections.
    // With --cpu they are pinned to CPUs --cpu-step apart, starting at --cpu.
    let (mut threads, mut cpu_step) = (1u32, 1usize);
    while let Some(k) = args.next() {
        let v: u64 = args
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| die(format_args!("bad value for {k}")));
        match k.as_str() {
            "--conns" => conns = v as u32,
            "--size" => size = v as u32,
            "--secs" => secs = v,
            "--cpu" => cpu = Some(v as usize),
            "--threads" => threads = (v as u32).max(1),
            "--cpu-step" => cpu_step = v as usize,
            _ => die(format_args!("unknown flag {k}")),
        }
    }
    let place = Placement {
        threads,
        cpu,
        cpu_step,
    };
    match cmd.as_str() {
        "selftest" => selftest(size),
        "badcrc" => bad_checksum(size),
        "multipart" => multipart(size),
        "objects" => objects(size),
        "put" => bench(true, conns, size, secs, place, Checksum::None),
        "put-crc" => bench(true, conns, size, secs, place, Checksum::Compute),
        "get" => bench(false, conns, size, secs, place, Checksum::None),
        "put-new" | "get-range" | "erase" | "list-keys" => ops(&cmd, conns, size, secs, cpu),
        other => die(format_args!("unknown command {other}")),
    }
}
