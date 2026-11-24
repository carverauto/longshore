// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A log and its replicas on one backend, under the allocation guard: the
//! simulator and the portable backend receive a frame, write it with
//! direct I/O, sync it and send it on to three peers without allocating.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::Limits;
use longshore_common::sim::{DiskConfig, Faults, SimIo};
use longshore_tcp::check::storage::{run_all, Replicated};
use longshore_tcp::Addr;

#[global_allocator]
static ALLOC: Guard = Guard;

#[cfg(test)]
fn sim(limits: Limits, seed: u64, faults: Faults) -> SimIo {
    let mut io = SimIo::new(limits, seed, faults);
    io.setup_disk(
        DiskConfig::BASE
            .with_max_files(4)
            .with_max_dirs(4)
            .with_max_bytes_per_file(1 << 20)
            .with_max_name_len(256),
    );
    io
}

#[cfg(test)]
fn sim_addr() -> Addr {
    Addr::Ip("192.0.2.7:7000".parse().unwrap())
}

#[test]
fn simulator_logs_and_replicates_under_network_faults() {
    for seed in 0..10 {
        let faults = Faults::NONE
            .with_partial_recv(400_000)
            .with_short_send(300_000);
        run_all(&mut |l| sim(l, seed, faults), sim_addr(), "/log.dat");
    }
}

#[test]
fn simulator_logs_and_replicates_without_allocating() {
    let mut r = Replicated::new(&mut |l| sim(l, 3, Faults::NONE), sim_addr(), "/log.dat");
    r.round(0);
    sealed(|| (1..100).for_each(|i| r.round(i)));
}

#[test]
fn real_sockets_and_a_real_file_log_and_replicate_without_allocating() {
    let name = format!("longshore-tcp-log-{}.dat", std::process::id());
    let path = std::env::temp_dir().join(name);
    let addr = Addr::Ip("127.0.0.1:0".parse().unwrap());
    let mut r = Replicated::new(
        &mut |l| PosixIo::new(l).unwrap(),
        addr,
        path.to_str().unwrap(),
    );
    r.round(0);
    sealed(|| (1..100).for_each(|i| r.round(i)));
    drop(r);
    std::fs::remove_file(&path).unwrap();
}
