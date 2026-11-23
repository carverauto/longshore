// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 3: `Virtual` world & `SimIo` trace parity (`#41`).

use super::f03_sim_parity::{drain_exact, run_all_ops_trace, sim_limits};
use crate::e2e::harness::drive_until;
use longshore::io::{BufId, Completions, Io, Op, Sock, Token, ECONNREFUSED, ENOENT};
use longshore::sim::{Faults, SimIo};
use std::io::ErrorKind;
use std::net::SocketAddr;

#[test]
fn t2_f03_1_zero_faults_draws_zero_fault_rng() {
    // With Faults::default(), `hold` and fault sites must not perturb RNG draws.
    let (log_a, h_a) = run_all_ops_trace(7, Faults::default());
    let (log_b, h_b) = run_all_ops_trace(7, Faults::default());
    assert_eq!(log_a, log_b);
    assert_eq!(h_a, h_b);
    assert_eq!(h_a, 189_675_162_845_082_480u64);
}

#[test]
fn t2_f03_2_stall_ticks_deadlock_timed_out() {
    let mut io = SimIo::new(sim_limits(), 1, Faults::default());
    let mut out = Completions::new(8);
    let addr: SocketAddr = "192.0.2.1:443".parse().unwrap();
    let _: SocketAddr = io.listen(Sock(0), addr).unwrap();
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(2),
            addr,
        },
    )
    .unwrap();
    let _: Vec<(u64, i32)> = drain_exact(&mut io, &mut out, 2);
    io.submit(
        Token(3),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let err = io.tick(true, &mut out).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut);
    assert!(err.to_string().contains("deadlock"), "got: {err}");
}

#[test]
fn t2_f03_3_read_file_absent_returns_enoent() {
    let mut io = SimIo::new(sim_limits(), 5, Faults::default());
    let mut out = Completions::new(8);
    let f = io.register_file("missing.conf").unwrap();
    io.set_file(f, None);
    io.submit(
        Token(1),
        Op::ReadFile {
            file: f,
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
    assert_eq!(cs[0].result, -ENOENT);

    io.set_file(f, Some(b"ok"));
    io.submit(
        Token(2),
        Op::ReadFile {
            file: f,
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
    assert_eq!(cs[0].result, 2);
    assert_eq!(&io.buf(BufId(0))[..2], b"ok");
}

#[test]
fn t2_f03_4_duplicate_listen_returns_addr_in_use() {
    let mut io = SimIo::new(sim_limits(), 1, Faults::default());
    let addr: SocketAddr = "192.0.2.5:8080".parse().unwrap();
    let _: SocketAddr = io.listen(Sock(0), addr).unwrap();
    let err = io.listen(Sock(1), addr).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AddrInUse);
}

#[test]
fn t2_f03_5_connect_refused_fault_leaves_slot_reusable() {
    let addr: SocketAddr = "192.0.2.8:443".parse().unwrap();
    let mut io = SimIo::new(
        sim_limits(),
        11,
        Faults {
            connect_refused: 1_000_000,
            ..Default::default()
        },
    );
    let mut out = Completions::new(8);
    let _: SocketAddr = io.listen(Sock(0), addr).unwrap();
    io.submit(
        Token(1),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
    assert_eq!(cs[0].result, -ECONNREFUSED);
    // Slot must remain free for immediate reuse without Close.
    io.submit(
        Token(2),
        Op::ConnectUdp {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
    assert_eq!(cs[0].result, 0);
}
