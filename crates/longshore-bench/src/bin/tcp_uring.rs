// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the shared `longshore::tcp` checks on real `io_uring` and on the
//! portable backend of the same kernel, and compares what the two carried.
//! Then a log and its replicas on each: frames received, written with
//! direct I/O to a file in `--dir`, synced and sent to three peers, under
//! the allocation guard.
//!
//!   tcp_uring [--v6] [--dir <directory on a file system with O_DIRECT>]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("tcp_uring needs Linux (io_uring / epoll)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

/// A hundred rounds of the log and its replicas, all but the first sealed.
#[cfg(target_os = "linux")]
fn log_and_replicate<IO: longshore::io::Io>(
    make: &mut impl FnMut(longshore::io::Limits) -> IO,
    addr: std::net::SocketAddr,
    path: &str,
) {
    use longshore::io::alloc_guard::sealed;
    use longshore::tcp::check::storage::Replicated;
    let mut r = Replicated::new(make, longshore::tcp::Addr::Ip(addr), path);
    r.round(0);
    sealed(|| (1..100).for_each(|i| r.round(i)));
    drop(r);
    // Best-effort cleanup: a leftover file does not change the result.
    let _: Option<()> = std::fs::remove_file(path).ok();
}

#[cfg(target_os = "linux")]
fn main() {
    use die::OrDie;
    use longshore::io::posix::PosixIo;
    use longshore::io::uring::UringIo;
    use longshore::tcp::check;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let mut v6 = false;
    let mut dir = std::env::temp_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--v6" => v6 = true,
            "--dir" => dir = args.next().or_die("--dir needs a directory").into(),
            other => die::die(format_args!("unknown flag {other}")),
        }
    }
    let ip = if v6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let addr = SocketAddr::new(ip, 0);
    let path = |tag: &str| {
        let name = format!("longshore-tcp-{tag}-{}.sock", std::process::id());
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    };

    let ring = check::run_all(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
        &path("ring"),
    );
    let portable = check::run_all(
        &mut |limits| PosixIo::new(limits).or_die("PosixIo::new"),
        addr,
        &path("posix"),
    );
    if ring.is_empty() || ring != portable {
        die::die("the ring and the portable backend carried different frames");
    }
    let log = |tag: &str| {
        let name = format!("longshore-tcp-{tag}-{}.dat", std::process::id());
        dir.join(name).to_string_lossy().into_owned()
    };
    log_and_replicate(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
        &log("ring"),
    );
    log_and_replicate(
        &mut |limits| PosixIo::new(limits).or_die("PosixIo::new"),
        addr,
        &log("posix"),
    );
    println!("ok tcp_uring v6={v6} frames={} logged=200", ring.len());
}
