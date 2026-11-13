// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Waiting for buffers and operation slots, and what callers see when an
//! I/O lane fails.

#[path = "../store/common/mod.rs"]
mod common;
mod rig;
use longshore::client::{Buffer, Error};
use rig::*;
use std::future::Future;
use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

#[test]
fn a_waiter_polled_twice_leaves_the_next_buffer_to_the_other_waiter() {
    let mut r = rig(1);
    let (held0, held1) = (r.buffer(), r.buffer());
    let (b, wb) = counted();
    let (_a, wa) = counted();
    let (mut cb, mut ca) = (Context::from_waker(&wb), Context::from_waker(&wa));
    let client = r.client.clone();
    let mut first = pin!(client.buffer());
    let mut second = pin!(client.buffer());
    assert!(first.as_mut().poll(&mut cb).is_pending());
    // The second waiter is polled again while it waits, as under `select!`.
    assert!(second.as_mut().poll(&mut ca).is_pending());
    assert!(second.as_mut().poll(&mut ca).is_pending());
    drop(held0);
    let _taken = match second.as_mut().poll(&mut ca) {
        Poll::Ready(buffer) => buffer,
        Poll::Pending => panic!("a buffer was free"),
    };
    // The first waiter is polled whenever it is woken, as an executor would.
    if b.get() > 0 {
        assert!(first.as_mut().poll(&mut cb).is_pending());
    }
    let before = b.get();
    drop(held1);
    assert!(b.get() > before, "a buffer came back and its waiter slept on");
    assert!(first.as_mut().poll(&mut cb).is_ready());
}

#[test]
fn a_buffer_returned_on_another_lane_wakes_a_waiter() {
    let mut r = rig(2);
    // Two buffers per lane, taken from lane 0 first.
    let mut bufs: Vec<Buffer> = (0..4).map(|_| r.buffer()).collect();
    let (woken, waker) = counted();
    let mut cx = Context::from_waker(&waker);
    let client = r.client.clone();
    let mut waiting = pin!(client.buffer());
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    drop(bufs.pop());
    assert_eq!(woken.get(), 1, "a buffer came back on lane 1 and woke nobody");
    assert!(waiting.as_mut().poll(&mut cx).is_ready());
}

#[test]
fn a_blocking_borrow_takes_a_buffer_returned_on_another_lane() {
    let mut r = rig(2);
    let mut bufs: Vec<Buffer> = (0..4).map(|_| r.buffer()).collect();
    let client = r.client.clone();
    let (got, arrived) = mpsc::channel();
    let borrower = std::thread::spawn(move || {
        let buffer = client.buffer_blocking();
        got.send(()).unwrap();
        drop(buffer);
    });
    // Let the borrower find every buffer out and block.
    std::thread::sleep(Duration::from_millis(100));
    drop(bufs.pop());
    let took = arrived.recv_timeout(Duration::from_secs(5));
    // Unblock the borrower before failing, so the rig outlives it.
    drop(bufs);
    borrower.join().unwrap();
    assert!(took.is_ok(), "a buffer came back on lane 1 and the borrower slept on");
}

#[test]
fn an_operation_in_flight_when_the_io_fails_ends_closed_with_its_buffer() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 3);
    let sent = body.as_slice()[..LEN].to_vec();
    let mut cx = Context::from_waker(Waker::noop());
    let mut storing = pin!(r.client.store("k", body, LEN));
    assert!(storing.as_mut().poll(&mut cx).is_pending());
    // The worker hands the store to the I/O, and the I/O then fails.
    let l = &mut r.lanes[0];
    l.worker.turn(&mut l.io);
    assert!(storing.as_mut().poll(&mut cx).is_pending());
    l.worker.abandon();
    let (result, body) = match storing.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("an abandoned operation is still pending"),
    };
    assert_eq!(result, Err(Error::Closed));
    assert_eq!(&body.as_slice()[..LEN], &sent[..]);
    // Nothing more is sent to the failed I/O.
    assert_eq!(r.run(r.client.erase("k")), Err(Error::Closed));
}

#[test]
fn every_operation_waiting_for_a_slot_ends_closed_when_the_io_fails() {
    // One operation slot: one erase in flight and two waiting for the slot.
    let mut r = rig_with(1, 1);
    let client = r.client.clone();
    let (a, b, c) = (counted(), counted(), counted());
    let mut ca = Context::from_waker(&a.1);
    let mut cb = Context::from_waker(&b.1);
    let mut cc = Context::from_waker(&c.1);
    let mut first = pin!(client.erase("a"));
    let mut second = pin!(client.erase("b"));
    let mut third = pin!(client.erase("c"));
    assert!(first.as_mut().poll(&mut ca).is_pending());
    let l = &mut r.lanes[0];
    l.worker.turn(&mut l.io);
    assert!(second.as_mut().poll(&mut cb).is_pending());
    assert!(third.as_mut().poll(&mut cc).is_pending());
    l.worker.abandon();
    assert_eq!(first.as_mut().poll(&mut ca), Poll::Ready(Err(Error::Closed)));
    // Each waiter is polled once woken, as an executor would.
    assert!(b.0.get() > 0, "the freed slot woke nobody");
    assert_eq!(second.as_mut().poll(&mut cb), Poll::Ready(Err(Error::Closed)));
    assert!(c.0.get() > 0, "the last waiter was left asleep");
    assert_eq!(third.as_mut().poll(&mut cc), Poll::Ready(Err(Error::Closed)));
}

#[test]
fn with_one_lane_failed_work_goes_to_the_lane_still_open() {
    let mut r = rig(2);
    r.lanes[0].worker.abandon();
    assert_eq!(r.run(r.client.erase("k")), Ok(()));
    let mut body = r.buffer();
    fill(&mut body, 7);
    let (stored, _body) = r.run(r.client.store("on-open-lane", body, LEN));
    assert_eq!(stored, Ok(()));
    assert_eq!(
        r.lanes[1].server.object_len("/on-open-lane"),
        Some(LEN as u32)
    );
}

#[test]
fn once_every_lane_is_closed_a_waiter_takes_a_closed_lanes_buffer() {
    let mut r = rig(2);
    // Two buffers per lane, taken from lane 0 first.
    let mut bufs: Vec<Buffer> = (0..4).map(|_| r.buffer()).collect();
    r.lanes[0].worker.abandon();
    let (woken, waker) = counted();
    let mut cx = Context::from_waker(&waker);
    let client = r.client.clone();
    // It waits on lane 1 alone, the only lane open.
    let mut waiting = pin!(client.buffer());
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    r.lanes[1].worker.abandon();
    if woken.get() > 0 {
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
    }
    let before = woken.get();
    drop(bufs.remove(0));
    assert!(woken.get() > before, "a buffer came back on lane 0 and woke nobody");
    assert!(waiting.as_mut().poll(&mut cx).is_ready());
}

#[test]
fn an_erase_asked_for_before_its_lane_failed_runs_on_another() {
    let mut r = rig(2);
    let erasing = r.client.erase("k");
    // Both lanes are idle, so either could have been picked: close each in
    // turn and the erase still finds the other.
    r.lanes[0].worker.abandon();
    assert_eq!(r.run(erasing), Ok(()));

    let mut r = rig(2);
    let erasing = r.client.erase("k");
    r.lanes[1].worker.abandon();
    assert_eq!(r.run(erasing), Ok(()));
}

#[test]
fn a_lane_failing_wakes_the_erases_waiting_for_its_slots() {
    // One slot per lane, each held by an operation nobody collects.
    let mut r = rig_with(2, 1);
    let client = r.client.clone();
    let mut cx = Context::from_waker(Waker::noop());
    // Each is sent before the next is asked for, so they take both lanes.
    let mut held = Vec::new();
    for i in 0..2 {
        let mut h = Box::pin(client.erase(&format!("h{i}")));
        assert!(h.as_mut().poll(&mut cx).is_pending());
        held.push(h);
    }
    let (woken, waker) = counted();
    let mut cw = Context::from_waker(&waker);
    let mut waiting = pin!(client.erase("k"));
    assert!(waiting.as_mut().poll(&mut cw).is_pending());
    r.lanes[0].worker.abandon();
    r.lanes[1].worker.abandon();
    assert!(woken.get() > 0, "the lanes closed under a waiting erase and woke nobody");
    assert_eq!(waiting.as_mut().poll(&mut cw), Poll::Ready(Err(Error::Closed)));
}
