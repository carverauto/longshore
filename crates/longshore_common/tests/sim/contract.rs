// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the shared `Io` contract suite against `SimIo`.

use longshore_common::io::contract;
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[test]
fn sim_keeps_the_shared_io_contract() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
    contract::run_all(&mut |limits| SimIo::new(limits, 1, Faults::default()), addr);
}

#[test]
fn sim_keeps_the_shared_io_contract_under_delay() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
    let faults = Faults {
        delay: 200_000,
        short_send: 200_000,
        partial_recv: 200_000,
        ..Faults::default()
    };
    contract::run_all(&mut |limits| SimIo::new(limits, 42, faults), addr);
}
