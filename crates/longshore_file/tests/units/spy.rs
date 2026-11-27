// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `check::Spy`, the backend the contract suite sees through: room it
//! takes away is given back when the backend itself is busy, it records
//! every completion of a tick in order however many come, and it passes
//! every call of the backend on.

use crate::rig::base;
use longshore_common::io::{Completions, Io, Limits, ListenOpts, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_file::check::Spy;
use longshore_file::{FileConfig, FileSizes};

fn limits() -> Limits {
    FileConfig::plan(base(), FileSizes::new(1, 1)).unwrap().0
}

fn timer(n: u64) -> (Token, Op) {
    (Token::new(n), Op::Timeout { nanos: 1000 })
}

/// Ticks until `n` completions have come.
fn drain<IO: Io>(io: &mut IO, out: &mut Completions, n: usize) {
    for _ in 0..100 {
        io.tick(true, out).unwrap();
        if out.len() >= n {
            return;
        }
    }
    panic!("{} of {n} completions came", out.len());
}

#[test]
fn room_is_given_back_when_the_backend_is_busy() {
    let sim = SimIo::new(limits().with_ops_max(1), 7, Faults::NONE);
    let mut spy = Spy::new(sim, 64);
    spy.set_room(Some(2));
    let (t0, op0) = timer(1);
    let (t1, op1) = timer(2);
    assert!(spy.submit(t0, op0).is_ok());
    // The backend's own room is out; the spy's is not taken by it.
    assert!(spy.submit(t1, op1).is_err());
    assert!(spy.submit_chain(&[(t1, op1)]).is_err());
    assert_eq!(spy.refused(), 0, "refused by the backend, not the spy");
    let mut out = Completions::new(8);
    drain(&mut spy, &mut out, 1);
    assert!(spy.submit(t1, op1).is_ok(), "the spy's second room is left");
    assert_eq!(spy.submitted(), 2);
}

#[test]
fn every_completion_of_a_tick_is_recorded_in_order() {
    let n = 100usize;
    let sim = SimIo::new(limits().with_ops_max(128), 7, Faults::NONE);
    let mut spy = Spy::new(sim, 256);
    for k in 0..n as u64 {
        let (t, op) = timer(k);
        assert!(spy.submit(t, op).is_ok());
    }
    let mut out = Completions::new(128);
    drain(&mut spy, &mut out, n);
    assert_eq!(spy.completed(), n as u64);
    assert_eq!(spy.outstanding(), 0);
    for k in 0..n as u64 {
        let c = out.pop().unwrap();
        let seen = spy.result(k).map(|(t, _)| t);
        assert_eq!(seen, Some(c.token()), "completion {k} in its place");
    }
}

#[test]
fn calls_with_a_default_are_passed_on() {
    let sim = SimIo::new(limits().with_sockets_max(1), 7, Faults::NONE);
    let mut spy = Spy::new(sim, 64);
    let opts = ListenOpts::DEFAULT.with_backlog(8);
    let addr = "127.0.0.1:9000".parse().unwrap();
    // The trait's own default refuses options; the simulator takes them.
    assert!(spy.listen_with(Sock::new(0), addr, opts).is_ok());
    assert_eq!(spy.waker().is_some(), spy.inner_mut().waker().is_some());
    assert!(!spy.take_ring_full());
}
