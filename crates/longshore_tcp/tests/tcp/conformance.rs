// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The transport conformance suite on the kernel transport: on the
//! simulator, with a closed socket's reports in every order by seed, and
//! on the portable backend. The ring runs it from `tcp_uring`.

use longshore_common::io::posix::PosixIo;
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::check::conformance::run_all;
use std::net::SocketAddr;

#[test]
fn the_kernel_transport_conforms_on_the_simulator_in_every_order_of_reports() {
    let addr: SocketAddr = "192.0.2.7:7000".parse().unwrap();
    for seed in 0..200 {
        let faults = Faults::NONE.with_reorder(400_000);
        run_all(&mut |limits| SimIo::new(limits, seed, faults), addr);
    }
    run_all(&mut |limits| SimIo::new(limits, 1, Faults::NONE), addr);
}

#[test]
fn the_kernel_transport_conforms_on_real_sockets() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    run_all(&mut |limits| PosixIo::new(limits).unwrap(), addr);
}
