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

    let listener =
        TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).or_die("bind");
    let addr = listener.local_addr().or_die("local_addr");
    let mut io = UringIo::new(
        Limits::BASE
            .with_sockets_max(2)
            .with_buffers(1)
            .with_buffer_size(64)
            .with_ops_max(4),
    )
    .or_die("UringIo::new");
    io.no_delay(true);
    io.keep_alive(true);
    io.inject(Fault::RingLost);

    let mut out = Completions::new(4);
    let c = sealed(|| {
        io.submit(
            Token::new(1),
            Op::Connect {
                sock: Sock::new(0),
                addr,
            },
        )
        .or_die("submit");
        io.tick(false, &mut out).or_die("tick");
        out.pop().or_die("connect failure completion")
    });
    assert_eq!(c.token(), Token::new(1));
    assert!(!c.more() && c.buf().is_none());
    assert!(
        c.result() < 0 && c.result() != -EINVAL,
        "expected true negative OS errno without EINVAL masking, got {}",
        c.result()
    );
}

#[cfg(target_os = "linux")]
fn check_armed_accept_empty_range() {
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::UringIo;
    use longshore::io::{Completion, Completions, Io, Limits, Op, Sock, Token, ENFILE};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let mut io = UringIo::new(
        Limits::BASE
            .with_sockets_max(2)
            .with_buffers(1)
            .with_buffer_size(64)
            .with_ops_max(4),
    )
    .or_die("UringIo::new");
    let _bound = io
        .listen(
            Sock::new(0),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .or_die("listen");
    let mut out = Completions::new(4);
    let c = sealed(|| {
        io.submit(
            Token::new(2),
            Op::ArmedAccept {
                listener: Sock::new(0),
            },
        )
        .or_die("submit");
        io.tick(false, &mut out).or_die("tick");
        out.pop().or_die("armed accept failure completion")
    });
    assert_eq!(c, Completion::new(Token::new(2), -ENFILE));
}

#[cfg(target_os = "linux")]
fn main() {
    check_connect_ring_lost();
    check_armed_accept_empty_range();
    println!("ok uring_faults");
}
