// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::io::{BufId, Io, ECANCELED, ENOENT};

mod mock;

use mock::MockWorld;

fn limits(ops_max: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(8)
        .with_buffers(4)
        .with_buffer_size(64)
        .with_ops_max(ops_max)
}

fn recv_op(sock: u32) -> Op {
    Op::Recv {
        sock: Sock(sock),
        buf: BufId(0),
        off: 0,
        len: 64,
    }
}

#[test]
fn armed_op_stays_pending_and_delivers_at_most_one_completion_per_step() {
    let mut world = MockWorld::new(0);
    world.queues[0].push_back(Attempt {
        result: 12,
        more: true,
        buf: Some(BufId(1)),
        off: 0,
    });
    world.queues[0].push_back(Attempt {
        result: 5,
        more: false,
        buf: Some(BufId(2)),
        off: 0,
    });
    let mut engine = Engine::with_world(limits(1), world);
    engine.submit(Token(10), recv_op(0)).unwrap();

    // Slot stays occupied while `more: true`.
    assert!(engine.submit(Token(11), recv_op(1)).is_err());

    let mut out = Completions::new(8);
    engine.tick(false, &mut out).unwrap();
    assert_eq!(
        out.pop(),
        Some(Completion {
            token: Token(10),
            result: 12,
            more: true,
            buf: Some(BufId(1)),
            off: 0,
        })
    );
    assert!(out.is_empty());
    assert!(engine.submit(Token(11), recv_op(1)).is_err());

    engine.tick(false, &mut out).unwrap();
    assert_eq!(
        out.pop(),
        Some(Completion {
            token: Token(10),
            result: 5,
            more: false,
            buf: Some(BufId(2)),
            off: 0,
        })
    );
    assert!(out.is_empty());
    // Final completion freed the op slot.
    assert!(engine.submit(Token(11), recv_op(1)).is_ok());
}

#[test]
fn swap_remove_and_wrap_never_double_delivers_an_armed_op_in_one_step() {
    // Pending layout: [A(armed), B(single timeout), C(idle armed), D(armed)] with start = 3.
    let mut world = MockWorld::new(3);
    world.queues[0].push_back(Attempt {
        result: 1,
        more: true,
        buf: Some(BufId(0)),
        off: 0,
    });
    world.queues[0].push_back(Attempt {
        result: 2,
        more: true,
        buf: Some(BufId(1)),
        off: 0,
    });
    world.queues[3].push_back(Attempt {
        result: 3,
        more: true,
        buf: Some(BufId(2)),
        off: 0,
    });
    world.queues[3].push_back(Attempt {
        result: 4,
        more: true,
        buf: Some(BufId(3)),
        off: 0,
    });
    let mut engine = Engine::with_world(limits(8), world);
    engine.submit(Token(1), recv_op(0)).unwrap();
    engine.submit(Token(2), Op::Timeout { nanos: 0 }).unwrap();
    engine.submit(Token(3), recv_op(2)).unwrap();
    engine.submit(Token(4), recv_op(3)).unwrap();

    let mut out = Completions::new(8);
    engine.tick(false, &mut out).unwrap();

    let mut seen = Vec::new();
    while let Some(c) = out.pop() {
        seen.push((c.token.0, c.result, c.more, c.buf));
    }
    // D (token 4), A (token 1), B (token 2): each visited once; D was NOT delivered twice.
    assert_eq!(
        seen,
        vec![
            (4, 3, true, Some(BufId(2))),
            (1, 1, true, Some(BufId(0))),
            (2, 0, false, None),
        ]
    );

    // Next step delivers the second completions for A and D.
    engine.tick(false, &mut out).unwrap();
    let mut second = Vec::new();
    while let Some(c) = out.pop() {
        second.push((c.token.0, c.result, c.more, c.buf));
    }
    assert_eq!(second.len(), 2);
    assert!(second.contains(&(1, 2, true, Some(BufId(1)))));
    assert!(second.contains(&(4, 4, true, Some(BufId(3)))));
}

#[test]
fn cancel_and_close_end_armed_ops_with_final_canceled_completion() {
    let mut world = MockWorld::new(0);
    world.queues[0].push_back(Attempt {
        result: 7,
        more: true,
        buf: Some(BufId(0)),
        off: 0,
    });
    world.queues[0].push_back(Attempt {
        result: 8,
        more: true,
        buf: Some(BufId(1)),
        off: 0,
    });
    world.queues[1].push_back(Attempt {
        result: 9,
        more: true,
        buf: Some(BufId(2)),
        off: 0,
    });
    let mut engine = Engine::with_world(limits(8), world);
    engine.submit(Token(10), recv_op(0)).unwrap();
    engine.submit(Token(20), recv_op(1)).unwrap();

    let mut out = Completions::new(8);
    engine.tick(false, &mut out).unwrap();
    assert_eq!(out.len(), 2);
    while out.pop().is_some() {}

    // Cancel token 10 and close Sock(1) (which owns token 20).
    engine
        .submit(Token(100), Op::Cancel { target: Token(10) })
        .unwrap();
    engine
        .submit(Token(101), Op::Close { sock: Sock(1) })
        .unwrap();
    engine.tick(false, &mut out).unwrap();

    let mut results = Vec::new();
    while let Some(c) = out.pop() {
        results.push((c.token.0, c.result, c.more, c.buf));
    }
    // Token 10 already delivered (8, more=true) before Cancel(10) ran in tick 1,
    // so its final -ECANCELED is deferred to tick 2 (at most 1 completion per step).
    assert!(results.contains(&(10, 8, true, Some(BufId(1)))));
    assert!(results.contains(&(100, 0, false, None)));
    assert!(results.contains(&(101, 0, false, None)));
    assert!(!results.contains(&(10, -ECANCELED, false, None)));

    engine.tick(false, &mut out).unwrap();
    let mut next_results = Vec::new();
    while let Some(c) = out.pop() {
        next_results.push((c.token.0, c.result, c.more, c.buf));
    }
    assert!(next_results.contains(&(10, -ECANCELED, false, None)));
    assert!(next_results.contains(&(20, -ECANCELED, false, None)));
    assert_eq!(engine.world().closed_socks, vec![Sock(1)]);

    // Token 10 was removed, so a second Cancel reports -ENOENT.
    engine
        .submit(Token(102), Op::Cancel { target: Token(10) })
        .unwrap();
    engine.tick(false, &mut out).unwrap();
    assert_eq!(out.pop(), Some(Completion::new(Token(102), -ENOENT)));
}

#[test]
fn full_completions_buffer_delivers_remaining_armed_completions_next_turn() {
    let mut world = MockWorld::new(0);
    world.queues[0].push_back(Attempt {
        result: 10,
        more: true,
        buf: Some(BufId(0)),
        off: 0,
    });
    world.queues[1].push_back(Attempt {
        result: 20,
        more: false,
        buf: Some(BufId(1)),
        off: 0,
    });
    let mut engine = Engine::with_world(limits(4), world);
    engine.submit(Token(1), recv_op(0)).unwrap();
    engine.submit(Token(2), recv_op(1)).unwrap();

    let mut out = Completions::new(1);
    engine.tick(true, &mut out).unwrap();
    assert_eq!(out.len(), 1);
    let first = out.pop().unwrap();

    engine.tick(true, &mut out).unwrap();
    assert_eq!(out.len(), 1);
    let second = out.pop().unwrap();

    assert_ne!(first.token, second.token);
}

#[test]
fn conditional_trace_hash_changes_only_when_more_or_buf_is_set() {
    let mut e1 = Engine::with_world(limits(2), MockWorld::new(0));
    let mut e2 = Engine::with_world(limits(2), MockWorld::new(0));
    let mut e3 = Engine::with_world(limits(2), MockWorld::new(0));

    e1.record_trace(Completion::new(Token(5), 42));
    e2.record_trace(Completion {
        token: Token(5),
        result: 42,
        more: true,
        buf: None,
        off: 0,
    });
    e3.record_trace(Completion {
        token: Token(5),
        result: 42,
        more: false,
        buf: Some(BufId(0)),
        off: 0,
    });

    assert_ne!(e1.trace_hash(), e2.trace_hash());
    assert_ne!(e1.trace_hash(), e3.trace_hash());
    assert_ne!(e2.trace_hash(), e3.trace_hash());
}

#[test]
fn close_cancels_armed_accept_and_armed_recv_and_give_back_forwards() {
    let l = limits(8)
        .with_accept_range_start(2)
        .with_accept_range_len(2)
        .with_ring_buffers(2)
        .with_ring_buffer_size(64);
    let mut engine = Engine::with_world(l, MockWorld::new(0));
    engine
        .submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    engine
        .submit(Token(2), Op::ArmedRecv { sock: Sock(1) })
        .unwrap();
    engine
        .submit(Token(3), Op::Close { sock: Sock(0) })
        .unwrap();
    engine
        .submit(Token(4), Op::Close { sock: Sock(1) })
        .unwrap();

    let mut out = Completions::new(8);
    engine.tick(false, &mut out).unwrap();
    let mut got = Vec::new();
    while let Some(c) = out.pop() {
        got.push((c.token.0, c.result, c.more, c.buf));
    }
    engine.tick(false, &mut out).unwrap();
    while let Some(c) = out.pop() {
        got.push((c.token.0, c.result, c.more, c.buf));
    }
    assert!(got.contains(&(1, -ECANCELED, false, None)), "{got:?}");
    assert!(got.contains(&(2, -ECANCELED, false, None)), "{got:?}");
    assert!(got.contains(&(3, 0, false, None)), "{got:?}");
    assert!(got.contains(&(4, 0, false, None)), "{got:?}");

    let rb = l.ring(1);
    engine.give_back(rb);
    assert_eq!(engine.world().returned_bufs, vec![rb]);
}

/// A zero-copy receive whose credit is used up ends with its last
/// completion though nothing more is ready to read: the engine ends it
/// itself, and asks the world nothing, as for a timer.
#[test]
fn a_zero_copy_receive_whose_credit_is_used_up_ends_with_nothing_ready() {
    let mut world = MockWorld::new(0);
    world.queues[0].push_back(Attempt {
        result: 100,
        more: true,
        buf: Some(BufId(4)),
        off: 0,
    });
    let mut engine = Engine::with_world(limits(2).with_area_pages(1), world);
    let recv = Op::ArmedRecvZc {
        sock: Sock(0),
        credit: 100,
    };
    engine.submit(Token(1), recv).unwrap();
    let mut out = Completions::new(8);
    let mut got = Vec::new();
    for _ in 0..4 {
        engine.tick(false, &mut out).unwrap();
        while let Some(c) = out.pop() {
            got.push((c.result, c.more));
        }
    }
    assert_eq!(got, [(100, true), (0, false)]);
}
