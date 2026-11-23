// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 2: `io::engine` (`Engine<W>`, `World`) (`#41`).

use super::f02_engine::base_limits;
use crate::e2e::harness::{drive_until, HookCall, OpKind, TestWorld};
use longshore::io::engine::Engine;
use longshore::io::{BufId, Busy, Completions, Io, Op, Sock, Token, ECANCELED};
use std::io;
use std::net::SocketAddr;

#[test]
fn t2_f02_1_busy_at_ops_max_does_not_touch_world() {
    let limits = base_limits().with_ops_max(2);
    let mut engine = Engine::with_world(limits, TestWorld::new());
    let mut out = Completions::new(8);
    engine
        .submit(Token::new(1), Op::Timeout { nanos: 0 })
        .unwrap();
    engine
        .submit(Token::new(2), Op::Timeout { nanos: 0 })
        .unwrap();
    engine.world().clear_calls();

    assert_eq!(
        engine.submit(Token::new(3), Op::Timeout { nanos: 0 }),
        Err(Busy)
    );
    assert!(engine.world().calls().is_empty());

    let cs = drive_until(&mut engine, &mut out, 5, |v| v.len() == 2).unwrap();
    assert_eq!(cs.len(), 2);
    engine.tick(false, &mut out).unwrap();
    assert!(out.is_empty());
}

#[test]
fn t2_f02_2_completions_capacity_one_spills_without_loss() {
    let mut engine = Engine::with_world(base_limits(), TestWorld::new());
    let mut out = Completions::new(1);
    for t in 1..=3 {
        engine
            .submit(Token::new(t), Op::Timeout { nanos: 0 })
            .unwrap();
    }

    let mut seen = Vec::new();
    for _ in 0..3 {
        engine.tick(false, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        seen.push(out.pop().expect("one").token().get());
    }
    engine.tick(false, &mut out).unwrap();
    assert!(out.is_empty());
    seen.sort();
    assert_eq!(seen, vec![1, 2, 3]);
}

#[test]
fn t2_f02_3_claimed_op_calls_release_on_cancel() {
    let world = TestWorld::new().with_claimed_socks(vec![Sock::new(0)]);
    let mut engine = Engine::with_world(base_limits(), world);
    let mut out = Completions::new(8);
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

    engine
        .submit(
            Token::new(10),
            Op::Connect {
                sock: Sock::new(0),
                addr,
            },
        )
        .unwrap();
    engine.tick(false, &mut out).unwrap();
    assert!(out.is_empty());

    engine
        .submit(
            Token::new(11),
            Op::Cancel {
                target: Token::new(10),
            },
        )
        .unwrap();
    let mut cs = drive_until(&mut engine, &mut out, 5, |v| v.len() == 2).unwrap();
    cs.sort_by_key(|c| c.token().get());
    assert_eq!((cs[0].token().get(), cs[0].result()), (10, -ECANCELED));
    assert_eq!((cs[1].token().get(), cs[1].result()), (11, 0));
    assert!(engine
        .world()
        .calls()
        .iter()
        .any(|c| matches!(c, HookCall::Release(OpKind::Connect))));
}

#[test]
fn t2_f02_4_wait_timed_out_propagates_cleanly() {
    let world = TestWorld::new().with_stall(true);
    let mut engine = Engine::with_world(base_limits(), world);
    let mut out = Completions::new(8);
    engine
        .submit(
            Token::new(1),
            Op::Recv {
                sock: Sock::new(0),
                buf: BufId::new(0),
                off: 0,
                len: 16,
            },
        )
        .unwrap();
    let err = engine.tick(true, &mut out).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(out.is_empty());
}

#[test]
fn t2_f02_5_close_invokes_world_closed_and_cancels_attached_ops() {
    let mut engine = Engine::with_world(base_limits(), TestWorld::new());
    let mut out = Completions::new(8);
    let addr: SocketAddr = "127.0.0.1:9090".parse().unwrap();

    engine
        .submit(
            Token::new(1),
            Op::Connect {
                sock: Sock::new(1),
                addr,
            },
        )
        .unwrap();
    engine
        .submit(
            Token::new(2),
            Op::Accept {
                listener: Sock::new(1),
                sock: Sock::new(2),
            },
        )
        .unwrap();
    engine.tick(false, &mut out).unwrap();
    assert!(out.is_empty());

    engine
        .submit(Token::new(3), Op::Close { sock: Sock::new(1) })
        .unwrap();
    let mut cs = drive_until(&mut engine, &mut out, 10, |v| v.len() == 3).unwrap();
    cs.sort_by_key(|c| c.token().get());
    assert_eq!(
        cs.iter()
            .map(|c| (c.token().get(), c.result()))
            .collect::<Vec<_>>(),
        vec![(1, -ECANCELED), (2, -ECANCELED), (3, 0)]
    );
    assert!(engine
        .world()
        .calls()
        .iter()
        .any(|c| matches!(c, HookCall::Closed(s) if *s == Sock::new(1))));
}
