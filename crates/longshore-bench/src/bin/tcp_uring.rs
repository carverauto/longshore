// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the shared `longshore::tcp` checks on real `io_uring` and on the
//! portable backend of the same kernel, and compares what the two carried.
//!
//!   tcp_uring [--v6]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("tcp_uring needs Linux (io_uring / epoll)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;

#[cfg(target_os = "linux")]
fn main() {
    use die::OrDie;
    use longshore::io::posix::PosixIo;
    use longshore::io::uring::UringIo;
    use longshore::tcp::check;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let mut v6 = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--v6" => v6 = true,
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
    println!("ok tcp_uring v6={v6} frames={}", ring.len());
}
