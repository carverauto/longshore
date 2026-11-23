// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial waiter, lane-close retry, buffer-drop, and `Budget`
//! abandon tests for `longshore_s3::client` (`GAP-C1`).

#[cfg(test)]
#[path = "../store/common/mod.rs"]
mod common;
#[expect(
    dead_code,
    reason = "these tests poll each operation by hand with `Rig::pump` and never fill a body, so `Rig::run`, `fill` and `LEN` go unused in this binary"
)]
#[cfg(test)]
mod rig;
use longshore_s3::client::{Budget, Error};
use longshore_s3::report::{Cause, End, Expect, Put};
use rig::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

fn once() -> Budget {
    Budget::once(Instant::now() + Duration::from_secs(3600))
}

#[test]
fn pending_dropped_after_void_abandon_and_in_slot_or_early_never_leaks() {
    let mut r = rig(1);
    // 1. Stage::Early dropped with borrowed buffer returns buffer cleanly.
    let b0 = r.buffer();
    let too_big = b0.len() + 1;
    drop(r.client.store("too-long", b0, too_big));
    let b0_back = r.buffer();

    // 2. Stage::Slot (op slot occupied) dropped with borrowed buffer returns buffer cleanly.
    let mut r1 = rig_with(1, 1);
    let mut cx = Context::from_waker(Waker::noop());
    let mut held_op = pin!(r1.client.erase("hold"));
    assert!(held_op.as_mut().poll(&mut cx).is_pending());
    let b_slot = r1.buffer();
    {
        let mut waiting = pin!(r1.client.store("queued", b_slot, 0));
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
    }
    let b_reclaimed = r1.buffer();
    drop((b_reclaimed, held_op, b0_back));

    // 3. Sent op abandoned with quiet=false BEFORE Pending::drop runs (never polled again).
    let b_void = r.buffer();
    {
        let mut sent = pin!(r.client.store("void-drop", b_void, 0));
        assert!(sent.as_mut().poll(&mut cx).is_pending());
        let l = &mut r.lanes[0];
        l.worker.turn(&mut l.io);
        l.worker.abandon(false);
    }
    let void = r.client.buffer_blocking();
    assert!(void.is_empty(), "void buffer must not be returned to pool");
}

#[test]
fn unbuffered_op_retries_other_lane_when_chosen_lane_closes_between_polls() {
    let mut r = rig_with(2, 1);
    let mut cx = Context::from_waker(Waker::noop());
    let mut h0 = pin!(r.client.erase("h0"));
    let mut h1 = pin!(r.client.erase("h1"));
    assert!(h0.as_mut().poll(&mut cx).is_pending());
    assert!(h1.as_mut().poll(&mut cx).is_pending());

    let mut target = pin!(r.client.erase("rerouted"));
    assert!(target.as_mut().poll(&mut cx).is_pending());

    // Close lane 0 while h1 completes on lane 1, freeing lane 1's slot for target.
    r.lanes[0].worker.abandon(true);
    while matches!(h1.as_mut().poll(&mut cx), Poll::Pending) {
        r.pump();
    }
    while matches!(target.as_mut().poll(&mut cx), Poll::Pending) {
        r.pump();
    }
    assert_eq!(h0.as_mut().poll(&mut cx), Poll::Ready(Err(Error::Closed)));
}

#[test]
fn worker_abandon_with_budget_report_ops_unsent_and_sent() {
    for quiet in [true, false] {
        // Unsent: abandoned before worker.turn() runs.
        let mut r = rig(1);
        let mut cx = Context::from_waker(Waker::noop());
        let body = r.buffer();
        let mut put_unsent = pin!(r.client.put("u", body, 0, &Put::default(), once()));
        let mut head_unsent = pin!(r.client.head("u", &Expect::default(), once()));
        assert!(put_unsent.as_mut().poll(&mut cx).is_pending());
        assert!(head_unsent.as_mut().poll(&mut cx).is_pending());

        r.lanes[0].worker.abandon(quiet);
        let Poll::Ready((res_p, b_ret)) = put_unsent.as_mut().poll(&mut cx) else {
            panic!("unsent put must resolve on abandon");
        };
        assert_eq!(res_p, Err(Error::Closed));
        assert_eq!(b_ret.is_empty(), !quiet);
        assert_eq!(
            head_unsent.as_mut().poll(&mut cx),
            Poll::Ready(Err(Error::Closed))
        );

        // Sent: request sent to stalled server, then abandon(quiet) -> End::Unknown(Cause::Closed).
        let mut r2 = rig(1);
        r2.lanes[0].server.script(common::Behavior::Stall);
        let body2 = r2.buffer();
        let mut get_sent = pin!(r2.client.get("g", None, &Expect::default(), body2, once()));
        assert!(get_sent.as_mut().poll(&mut cx).is_pending());
        for _ in 0..4 {
            r2.pump();
        }
        r2.lanes[0].worker.abandon(quiet);
        let Poll::Ready((res_g, b_ret2)) = get_sent.as_mut().poll(&mut cx) else {
            panic!("sent get must resolve on abandon");
        };
        let rep_g = res_g.expect("report op returns Ok(Report) on sent abandon");
        assert_eq!(rep_g.end, End::Unknown(Cause::Closed));
        assert_eq!(rep_g.sent, 1);
        assert_eq!(b_ret2.is_empty(), !quiet);
    }
}
