// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel sealed fault-injection checks for `UringIo`.
//!
//!   uring_faults

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("uring_faults needs Linux (io_uring)");
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
fn check_connect_ring_lost() {
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::{Fault, UringIo};
    use longshore::io::{Completions, Io, Limits, Op, Sock, Token, EINVAL};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut io = UringIo::new(Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 64,
        ops_max: 4,
        ..Limits::BASE
    })
    .unwrap();
    io.no_delay(true);
    io.keep_alive(true);
    io.inject(Fault::RingLost);

    let mut out = Completions::new(4);
    let c = sealed(|| {
        io.submit(
            Token(1),
            Op::Connect {
                sock: Sock(0),
                addr,
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();
        out.pop().or_die("connect failure completion")
    });
    assert_eq!(c.token, Token(1));
    assert!(!c.more && c.buf.is_none());
    assert!(
        c.result < 0 && c.result != -EINVAL,
        "expected true negative OS errno without EINVAL masking, got {}",
        c.result
    );
}

#[cfg(target_os = "linux")]
fn check_armed_accept_empty_range() {
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::UringIo;
    use longshore::io::{Completion, Completions, Io, Limits, Op, Sock, Token, ENFILE};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let mut io = UringIo::new(Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 64,
        ops_max: 4,
        ..Limits::BASE
    })
    .unwrap();
    let _bound = io
        .listen(Sock(0), SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .unwrap();
    let mut out = Completions::new(4);
    let c = sealed(|| {
        io.submit(Token(2), Op::ArmedAccept { listener: Sock(0) })
            .unwrap();
        io.tick(false, &mut out).unwrap();
        out.pop().or_die("armed accept failure completion")
    });
    assert_eq!(c, Completion::new(Token(2), -ENFILE));
}

#[cfg(target_os = "linux")]
fn main() {
    check_connect_ring_lost();
    check_armed_accept_empty_range();
    println!("ok uring_faults");
}
