// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Resolves hostnames through the in-tree resolver on real io_uring, using
//! the nameservers in /etc/resolv.conf. Allocation is sealed while it runs.
//!
//!   dns_uring HOST [HOST...]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dns_uring needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
fn main() {
    use longshore::dns::codec::parse_resolv_conf;
    use longshore::dns::{Config, Resolver};
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::UringIo;
    use longshore::io::{BufId, Completions, Io, Limits, Sock};
    use std::net::SocketAddrV4;

    let hosts: Vec<String> = std::env::args().skip(1).collect();
    if hosts.is_empty() {
        eprintln!("usage: dns_uring HOST [HOST...]");
        std::process::exit(2);
    }
    let conf = std::fs::read_to_string("/etc/resolv.conf").expect("read /etc/resolv.conf");
    let servers: Vec<SocketAddrV4> = parse_resolv_conf(&conf)
        .into_iter()
        .take(2)
        .map(|ip| SocketAddrV4::new(ip, 53))
        .collect();
    assert!(
        !servers.is_empty(),
        "no IPv4 nameserver in /etc/resolv.conf"
    );
    println!("nameservers: {servers:?}");

    let mut io = UringIo::new(Limits {
        sockets_max: 2,
        buffers: 3,
        buffer_size: 1024,
        small_buffers: 0,
        small_buffer_size: 0,
        ops_max: 8,
    })
    .unwrap_or_else(|e| {
        eprintln!("dns_uring: {e}");
        std::process::exit(1);
    });
    let n = hosts.len();
    let mut r = Resolver::new(Config {
        servers,
        hosts,
        refresh_ns: 30_000_000_000,
        timeout_ns: 1_000_000_000,
        socks: [Sock(0), Sock(1)],
        rx_bufs: [BufId(0), BufId(1)],
        tx_buf: BufId(2),
        token_base: 1 << 40,
    });
    let mut out = Completions::new(8);
    let t0 = io.now_ns();
    r.start(&mut io);
    sealed(|| {
        // One full pass over the hosts, or 5 seconds.
        while r.cycles == 0 && io.now_ns() - t0 < 5_000_000_000 {
            io.tick(true, &mut out).expect("tick");
            while let Some(c) = out.pop() {
                r.on_completion(&mut io, c);
            }
        }
    });
    let ms = (io.now_ns() - t0) as f64 / 1e6;
    for i in 0..n {
        println!("{:?}", r.addrs(i));
    }
    println!(
        "ready={} queries={} failures={} elapsed_ms={ms:.1} enters={}",
        r.ready(),
        r.queries,
        r.failures,
        io.enters()
    );
    std::process::exit(if r.ready() { 0 } else { 1 });
}
