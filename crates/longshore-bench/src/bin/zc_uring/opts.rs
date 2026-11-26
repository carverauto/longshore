// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The command line of `zc_uring`, and the backend it asks for.

use crate::die::{die, OrDie};
use longshore::io::uring::UringIo;
use longshore::io::{FileSlot, Io, Limits, Op, Token};
use std::net::SocketAddr;

pub(super) const PAGE: u32 = Limits::PAGE;
pub(super) const ACCEPT: u64 = 1;
pub(super) const RECV: u64 = 2;
pub(super) const CANCEL: u64 = 3;
pub(super) const OPEN: u64 = 4;
/// The gathered write in row `k` has token `WRITE + k`.
pub(super) const WRITE: u64 = 100;
pub(super) const LOG: FileSlot = FileSlot::new(0);

pub(super) struct Opts {
    /// `check`, `log` or `refuse`.
    pub(super) mode: String,
    pub(super) interface: u32,
    pub(super) queue: u32,
    pub(super) addr: SocketAddr,
    pub(super) frame: u32,
    pub(super) pages: u32,
    pub(super) file: Option<String>,
    pub(super) writes: u32,
    pub(super) spans: u32,
}

extern "C" {
    fn if_nametoindex(name: *const core::ffi::c_char) -> u32;
    fn clock_gettime(clock: i32, ts: *mut [i64; 2]) -> i32;
}

/// Submit `op`, or end the program: every operation was given room.
pub(super) fn submit(io: &mut UringIo, token: u64, op: Op) {
    if io.submit(Token::new(token), op).is_err() {
        die(format_args!("no room to submit {op:?}"));
    }
}

pub(super) fn interface(name: &str) -> u32 {
    let c = std::ffi::CString::new(name).or_die("an interface name without NUL");
    match unsafe { if_nametoindex(c.as_ptr()) } {
        0 => die(format_args!("there is no interface named {name}")),
        n => n,
    }
}

/// CPU time of the calling thread, in seconds.
pub(super) fn thread_cpu() -> f64 {
    let mut ts = [0i64; 2];
    // CLOCK_THREAD_CPUTIME_ID; a failure leaves zero, which reads as none.
    let _: i32 = unsafe { clock_gettime(3, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

pub(super) fn parse() -> Opts {
    let usage = "usage: zc_uring check|log|refuse <interface> <queue> <address:port> \
                 [--frame N] [--pages N] [--file PATH] [--writes N] [--spans N]";
    let mut args = std::env::args().skip(1);
    let mut next = |what: &str| args.next().or_die(&format!("{what} missing; {usage}"));
    let mode = next("the mode");
    if !["check", "log", "refuse"].contains(&mode.as_str()) {
        die(format_args!("no mode {mode}; {usage}"));
    }
    let mut o = Opts {
        mode,
        interface: interface(&next("the interface")),
        queue: next("the queue").parse().or_die("the queue, a number"),
        addr: next("the address")
            .parse()
            .or_die("the address, as 10.0.0.1:9311"),
        frame: 65_536,
        pages: 16_384,
        file: None,
        writes: 4,
        spans: 256,
    };
    while let Some(flag) = args.next() {
        let value = args.next().or_die(&format!("{flag} needs a value"));
        let n = || value.parse::<u32>().or_die(&format!("{flag}, a number"));
        match flag.as_str() {
            "--frame" => o.frame = n(),
            "--pages" => o.pages = n(),
            "--writes" => o.writes = n(),
            "--spans" => o.spans = n(),
            "--file" => o.file = Some(value),
            _ => die(format_args!("no option {flag}; {usage}")),
        }
    }
    if o.frame <= 256 || o.pages == 0 || o.writes == 0 || o.spans == 0 {
        die("--frame must be over 256, and --pages, --writes and --spans at least 1");
    }
    if o.mode == "log" && o.file.is_none() {
        die("log needs --file");
    }
    o
}

pub(super) fn limits(o: &Opts) -> Limits {
    Limits::BASE
        .with_sockets_max(2)
        .with_small_buffers(o.writes + 1)
        .with_small_buffer_size((12 * o.spans).max(4096))
        .with_files_max(1)
        .with_block_size(PAGE)
        .with_ops_max(o.writes + 8)
        .with_area_pages(o.pages)
        .with_area_queue(o.interface, o.queue)
        .with_gather(o.writes, o.spans)
}
