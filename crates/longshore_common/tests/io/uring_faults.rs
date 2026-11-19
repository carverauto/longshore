// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Sealed zero-allocation fault tests for `UringIo` on Linux.

#![cfg(all(target_os = "linux", feature = "fault-injection"))]

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::uring::{Fault, UringIo};
use longshore_common::io::{Completion, Completions, Io, Limits, Op, Sock, Token, EINVAL, ENFILE};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

#[global_allocator]
static ALLOC: Guard = Guard;

#[test]
fn connect_update_file_failure_under_ring_lost_is_sealed_and_preserves_errno() {
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
        out.pop().expect("connect failure completion")
    });
    assert_eq!(c.token, Token(1));
    assert!(!c.more && c.buf.is_none());
    assert!(
        c.result < 0 && c.result != -EINVAL,
        "expected true negative OS errno without EINVAL masking, got {}",
        c.result
    );
}

#[test]
fn armed_accept_with_empty_range_fails_enfile_without_allocation() {
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
        out.pop().expect("armed accept failure completion")
    });
    assert_eq!(c, Completion::new(Token(2), -ENFILE));
}
