// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The transport conformance suite on the kernel transport: on the
//! simulator, with a closed socket's reports in every order by seed, and
//! on the portable backend. The ring runs it from `tcp_uring`.

use longshore_common::io::posix::PosixIo;
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::check::conformance::{run_all, run_zc};
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
fn zero_copy_receives_conform_on_the_simulators_area_with_its_faults() {
    let addr: SocketAddr = "192.0.2.7:7000".parse().unwrap();
    for seed in 0..200 {
        let faults = Faults::NONE
            .with_reorder(400_000)
            .with_copied(100_000)
            .with_ring_full(100_000);
        run_zc(&mut |limits| SimIo::new(limits, seed, faults), addr);
    }
}

#[test]
fn the_kernel_transport_conforms_on_real_sockets() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    run_all(&mut |limits| PosixIo::new(limits).unwrap(), addr);
}

#[cfg(feature = "tls-core")]
#[test]
fn handshakes_are_reported_once_in_every_order_of_reports_and_on_real_sockets() {
    use longshore_tcp::check::conformance::run_all_secured;
    use longshore_tcp::check::Certs;
    use longshore_tls::handshake::{ClientSetup, ServerSetup};
    let name = "replica.longshore.test";
    let k = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    let (cert, key) = (k.cert.der().to_vec(), k.signing_key.serialize_der());
    let client = ClientSetup::with_roots(&[&cert]).unwrap();
    let server = ServerSetup::new(&[&cert], &key).unwrap();
    let certs = Certs::new(client, server, name);
    let addr: SocketAddr = "192.0.2.7:7000".parse().unwrap();
    for seed in 0..50 {
        let faults = Faults::NONE.with_reorder(400_000);
        run_all_secured(&mut |limits| SimIo::new(limits, seed, faults), addr, &certs);
    }
    let local: SocketAddr = "127.0.0.1:0".parse().unwrap();
    run_all_secured(&mut |limits| PosixIo::new(limits).unwrap(), local, &certs);
}
