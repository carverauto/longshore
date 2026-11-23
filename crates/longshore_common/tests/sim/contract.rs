// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the shared `Io` contract suite against `SimIo`.

use longshore_common::io::{contract, contract_net};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[test]
fn sim_keeps_the_shared_io_contract() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
    contract::run_all(&mut |limits| SimIo::new(limits, 1, Faults::default()), addr);
    contract_net::run_all(
        &mut |limits| SimIo::new(limits, 1, Faults::default()),
        addr,
        "/contract.sock",
    );
}

#[test]
fn sim_keeps_the_shared_io_contract_under_delay() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
    let faults = Faults::NONE
        .with_delay(200_000)
        .with_short_send(200_000)
        .with_partial_recv(200_000);
    contract::run_all(&mut |limits| SimIo::new(limits, 42, faults), addr);
    contract_net::run_all(
        &mut |limits| SimIo::new(limits, 42, faults),
        addr,
        "/contract-delay.sock",
    );
}
