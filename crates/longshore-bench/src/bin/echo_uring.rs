// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the echo scenario on real io_uring over loopback, with allocation
//! forbidden after setup. This is the real-kernel counterpart of the
//! simulator's echo test.
//!
//!   echo_uring [--total BYTES] [--chunk BYTES] [--zc] [--v6] [--measure]
//!
//! `--v6` runs the echo and the refused connect over the IPv6 loopback.
//! `--measure` runs the echo with segment sizes measured on connect.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("echo_uring needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;
#[cfg(target_os = "linux")]
use die::OrDie;

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
#[path = "echo_uring/checks.rs"]
mod checks;

/// What the command line asked for.
#[cfg(target_os = "linux")]
struct Args {
    total: u64,
    chunk: u32,
    zc: bool,
    v6: bool,
    measure: bool,
}

#[cfg(target_os = "linux")]
fn args() -> Args {
    let mut a = Args {
        total: 64u64 << 20,
        chunk: 1u32 << 20,
        zc: false,
        v6: false,
        measure: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--zc" => a.zc = true,
            "--v6" => a.v6 = true,
            "--measure" => a.measure = true,
            "--total" => {
                a.total = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .or_die("--total BYTES")
            }
            "--chunk" => {
                a.chunk = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .or_die("--chunk BYTES")
            }
            _ => die::die(format_args!("unknown flag {k}")),
        }
    }
    a
}

#[cfg(target_os = "linux")]
fn main() {
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::UringIo;
    use longshore::io::{Completions, Limits};
    use longshore::sim::scenario::echo;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let Args {
        total,
        chunk,
        zc,
        v6,
        measure,
    } = args();
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: chunk,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = match UringIo::new(limits) {
        Ok(io) => io,
        Err(e) => {
            eprintln!("echo_uring: {e}");
            std::process::exit(1);
        }
    };
    let mut out = Completions::new(8);
    // Timer and operation-limit self-checks before the echo.
    checks::timers(&mut io, &mut out);
    checks::files(&mut io, &mut out);
    checks::cancel(&mut io, &mut out);
    let loopback = if v6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    checks::failed_connect(&mut io, &mut out, loopback);
    checks::measured_connect(&mut io, &mut out, loopback, measure);
    let addr = SocketAddr::new(loopback, 0);
    let start = std::time::Instant::now();
    let r = sealed(|| echo(&mut io, addr, total, chunk, zc, &mut out));
    let secs = start.elapsed().as_secs_f64();
    match r {
        Ok(s) => println!("ok total={total} chunk={chunk} zero_copy={zc} sends={} recvs={} ticks={} enters={} secs={secs:.3}", s.sends, s.recvs, s.ticks, io.enters()),
        Err(e) => {
            eprintln!("echo_uring: FAILED {e:?}");
            std::process::exit(1);
        }
    }
}
