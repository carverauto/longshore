// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Verifies `PosixIo` performs zero heap allocations during the echo scenario.

#![cfg(unix)]

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::scenario::echo;
use longshore_common::io::{Completions, Limits};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_buffers(3)
    .with_buffer_size(4096)
    .with_ops_max(4);

#[test]
fn posix_echo_allocates_nothing_after_setup() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(8);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let stats = sealed(|| echo(&mut io, addr, 300_000, 4096, false, &mut out))
        .expect("echo under alloc_guard");
    assert!(stats.sends() > 0 && stats.recvs() > 0);
}
