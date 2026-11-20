// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel checks for armed accept/recv, buffer ring, socket options, and Unix sockets.
//!
//!   server_uring [--v6] [--posix]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("server_uring needs Linux (io_uring / epoll)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
#[path = "server_uring/checks.rs"]
mod checks;
#[cfg(target_os = "linux")]
#[path = "server_uring/options.rs"]
mod options;
#[cfg(target_os = "linux")]
#[path = "server_uring/thousand.rs"]
mod thousand;

#[cfg(target_os = "linux")]
fn raise_nofile() {
    #[repr(C)]
    struct Rlimit {
        rlim_cur: u64,
        rlim_max: u64,
    }
    const RLIMIT_NOFILE: i32 = 7;
    extern "C" {
        fn getrlimit(resource: i32, rlim: *mut Rlimit) -> i32;
        fn setrlimit(resource: i32, rlim: *const Rlimit) -> i32;
    }
    let mut lim = Rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe {
        if getrlimit(RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < 8192 {
            let target = lim.rlim_max.min(8192);
            if target > lim.rlim_cur {
                lim.rlim_cur = target;
                let _ = setrlimit(RLIMIT_NOFILE, &lim);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    raise_nofile();

    let mut v6 = false;
    let mut posix = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--v6" => v6 = true,
            "--posix" => posix = true,
            other => panic!("unknown flag {other}"),
        }
    }
    let ip = if v6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let addr = SocketAddr::new(ip, 0);

    if posix {
        checks::run_posix(addr);
    } else {
        checks::run_uring(addr);
    }

    println!("ok server_uring v6={v6} posix={posix}");
}
