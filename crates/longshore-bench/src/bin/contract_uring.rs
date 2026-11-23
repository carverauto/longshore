// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the shared `Io` contract checks on real `io_uring` (or `PosixIo` with
//! `--posix`), plus sealed zero-allocation echo and blocked cross-thread wake.
//!
//!   contract_uring [--v6] [--posix]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("contract_uring needs Linux (io_uring / epoll)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
fn run_backend<IO: longshore::io::Io>(
    make: &mut impl FnMut(longshore::io::Limits) -> IO,
    addr: std::net::SocketAddr,
) {
    use longshore::io::alloc_guard::sealed;
    use longshore::io::scenario::echo;
    use longshore::io::{contract, contract_net};
    use longshore::io::{Completions, Limits, Op, Token};

    contract::run_all(make, addr);
    let unix_path = format!("/tmp/longshore-contract-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&unix_path);
    contract_net::run_all(make, addr, &unix_path);
    let _ = std::fs::remove_file(&unix_path);

    // Cross-thread wake while `tick(true)` is blocked in the kernel.
    {
        let limits = Limits {
            sockets_max: 1,
            buffers: 1,
            buffer_size: 64,
            ops_max: 2,
            ..Limits::BASE
        };
        let mut io = make(limits);
        let mut out = Completions::new(4);
        let waker = io.waker().expect("waker");
        io.submit(Token(1), Op::AwaitWake).unwrap();
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(15));
            waker.wake();
        });
        io.tick(true, &mut out).expect("tick");
        let c = out.pop().expect("wake completion");
        assert_eq!((c.token.0, c.result), (1, 0));
        h.join().unwrap();
    }

    // Sealed zero-allocation echo check.
    {
        let limits = Limits {
            sockets_max: 4,
            buffers: 3,
            buffer_size: 4096,
            ops_max: 4,
            ..Limits::BASE
        };
        let mut io = make(limits);
        let mut out = Completions::new(8);
        let stats = sealed(|| echo(&mut io, addr, 300_000, 4096, false, &mut out))
            .unwrap_or_else(|e| panic!("sealed echo failed: {e:?}"));
        assert!(stats.sends > 0 && stats.recvs > 0);
    }
}

#[cfg(target_os = "linux")]
fn main() {
    use longshore::io::posix::PosixIo;
    use longshore::io::uring::UringIo;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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
        run_backend(
            &mut |limits| {
                PosixIo::new(limits).unwrap_or_else(|e| {
                    eprintln!("contract_uring --posix: {e}");
                    std::process::exit(1);
                })
            },
            addr,
        );
    } else {
        run_backend(
            &mut |limits| {
                UringIo::new(limits).unwrap_or_else(|e| {
                    eprintln!("contract_uring: {e}");
                    std::process::exit(1);
                })
            },
            addr,
        );
    }

    println!("ok contract_uring v6={v6} posix={posix}");
}
