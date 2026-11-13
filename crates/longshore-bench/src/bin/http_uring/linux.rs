// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The Linux implementation of this program.

mod selftest;
use selftest::*;
mod https;
use https::*;
mod sink;
use sink::*;
mod throughput;
use throughput::*;

use core::ffi::{c_int, c_void};
use longshore::http::client::{self, Client, Error, Handled, Request};
use longshore::http::server::{self, Server};
use longshore::io::alloc_guard::sealed;
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::fd::AsRawFd;

extern "C" {
    fn sched_setaffinity(pid: c_int, len: usize, mask: *const u64) -> c_int;
    fn clock_gettime(clk: c_int, ts: *mut [i64; 2]) -> c_int;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
}
const MSG_TRUNC: c_int = 0x20;
const HEARTBEAT: u64 = 1 << 50;
const MS: u64 = 1_000_000;

fn die(msg: &str) -> ! {
    eprintln!("http_uring: {msg}");
    std::process::exit(1)
}

fn thread_cpu_secs() -> f64 {
    let mut ts = [0i64; 2];
    unsafe { clock_gettime(3, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

fn core_busy_and_total(cpu: usize) -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    let tag = format!("cpu{cpu} ");
    let Some(line) = stat.lines().find(|l| l.starts_with(&tag)) else {
        return (0, 0);
    };
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    let total: u64 = v.iter().take(8).sum();
    (
        total - v.get(3).copied().unwrap_or(0) - v.get(4).copied().unwrap_or(0),
        total,
    )
}

struct Args {
    cmd: String,
    addr: String,
    port: u16,
    conns: u32,
    size: u32,
    secs: u64,
    cpu: Option<usize>,
    tls_cert: Option<String>,
    batch: u32,
    /// Checksum every PUT body with CRC-64/NVME.
    crc: bool,
}

pub fn main() {
    let mut a = Args {
        cmd: String::new(),
        addr: String::new(),
        port: 29200,
        conns: 8,
        size: 8 << 20,
        secs: 5,
        cpu: None,
        tls_cert: None,
        batch: 1 << 20,
        crc: false,
    };
    let mut it = std::env::args().skip(1);
    if std::env::args().nth(1).as_deref() == Some("https") {
        let v: Vec<String> = std::env::args().skip(2).collect();
        if v.len() < 2 {
            die("usage: http_uring https HOST IPV4 [PATH]");
        }
        return https(&v[0], &v[1], v.get(2).map_or("/", |p| p.as_str()));
    }
    a.cmd = it.next().unwrap_or_else(|| die("usage: http_uring selftest | sink --port P [--size N] | put|get --addr H:P[,H:P...] [--conns N] [--size N] [--secs S] [--cpu N] [--crc 1]"));
    while let Some(k) = it.next() {
        let v = it
            .next()
            .unwrap_or_else(|| die(&format!("missing value for {k}")));
        let num = || {
            v.parse::<u64>()
                .unwrap_or_else(|_| die(&format!("bad number for {k}: {v}")))
        };
        match k.as_str() {
            "--tls-cert" => a.tls_cert = Some(v),
            "--batch" => a.batch = num() as u32,
            "--addr" => a.addr = v,
            "--port" => a.port = num() as u16,
            "--conns" => a.conns = num() as u32,
            "--size" => a.size = num() as u32,
            "--secs" => a.secs = num(),
            "--cpu" => a.cpu = Some(num() as usize),
            "--crc" => a.crc = num() != 0,
            _ => die(&format!("unknown flag {k}")),
        }
    }
    match a.cmd.as_str() {
        "selftest" => selftest(),
        "sink" => sink(a.port, a.size, a.tls_cert.as_deref()),
        "put" => bench(&a, true),
        "get" => bench(&a, false),
        other => die(&format!("unknown command {other}")),
    }
}

fn client_cfg(conns: u32, buffer_size: u32, first_sock: u32) -> client::Config {
    client::Config {
        connections_max: conns,
        first_sock,
        first_buf: 0,
        head_max: 4096,
        buffer_size,
        max_age_ns: 3_600_000 * MS,
        max_requests: u32::MAX,
        request_timeout_ns: 30_000 * MS,
        zero_copy_min: 64 * 1024,
        token_base: 1 << 40,
        tls_batch: 1 << 20,
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    }
}
