// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The shared checks on the simulator and on the portable backend, which
//! must see the same frames. The ring runs them from `longshore-bench`
//! (`tcp_uring`) on a real kernel.

use longshore_common::io::posix::PosixIo;
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::check;
use std::net::SocketAddr;

#[cfg(test)]
fn unix_path(tag: &str) -> String {
    let dir = std::env::temp_dir();
    let name = format!("longshore-tcp-{tag}-{}.sock", std::process::id());
    dir.join(name).to_str().unwrap().to_string()
}

#[test]
fn the_simulator_and_real_sockets_carry_the_same_frames() {
    let sim_addr: SocketAddr = "192.0.2.7:7000".parse().unwrap();
    let sim = check::run_all(
        &mut |limits| SimIo::new(limits, 11, Faults::NONE),
        sim_addr,
        &unix_path("sim"),
    );
    let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let posix = check::run_all(
        &mut |limits| PosixIo::new(limits).unwrap(),
        loopback,
        &unix_path("posix"),
    );
    assert!(!sim.is_empty());
    assert!(sim == posix, "the two backends delivered different frames");
}

#[test]
fn the_checks_hold_under_partial_receives_and_short_sends() {
    for seed in 0..10 {
        let faults = Faults::NONE
            .with_partial_recv(400_000)
            .with_short_send(300_000);
        let addr: SocketAddr = "192.0.2.7:7000".parse().unwrap();
        let trace = check::run_all(
            &mut |limits| SimIo::new(limits, seed, faults),
            addr,
            &unix_path("faults"),
        );
        assert!(!trace.is_empty(), "seed {seed}");
    }
}
