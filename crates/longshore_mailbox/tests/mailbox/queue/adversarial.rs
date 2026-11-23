// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial tests for `QueueReceiver`, `Receiver` two-phase close,
//! `Busy` handling, and spurious `wake()` during `wait_stop()`.

use super::{test_limits, Duration};
use core::task::{Context, Poll, Waker};
use longshore_common::io::{Completion, Completions, Io, Op, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_mailbox::{mailbox_parked, queue, queue_parked, Config, Recv, SlotWaiter, TryRecv};

#[test]
fn receiver_arm_and_on_completion_handle_busy_foreign_tokens_and_post_close() {
    let limits = test_limits().with_ops_max(1);
    let mut io = SimIo::new(limits, 7, Faults::default());
    let (tx, mut rx) = queue::<u32, _>(&mut io, 2, Token::new(55)).unwrap();

    // Occupy the single op slot so rx.arm() hits Busy without setting armed=true.
    io.submit(Token::new(1), Op::Timeout { nanos: 1_000_000 })
        .unwrap();
    assert!(rx.arm(&mut io).is_err());

    let mut out = Completions::new(4);
    io.tick(true, &mut out).unwrap();
    let t_done = out.pop().unwrap();
    assert!(!rx.on_completion(&mut io, t_done).unwrap());
    assert!(rx.arm(&mut io).is_ok());

    tx.try_send(42).unwrap();
    io.tick(true, &mut out).unwrap();
    let wake_c = out.pop().unwrap();
    assert_eq!(wake_c.token(), Token::new(55));
    io.submit(Token::new(2), Op::Timeout { nanos: 1_000_000 })
        .unwrap();
    assert!(rx.on_completion(&mut io, wake_c).is_err());
    io.tick(true, &mut out).unwrap();
    let _: Option<Completion> = out.pop();
    assert!(rx.arm(&mut io).is_ok());

    // Close idempotency & post-close on_completion does not re-arm.
    let mut items = Vec::new();
    rx.close(|i| items.push(i));
    rx.close(|i| items.push(i));
    assert_eq!(items, vec![Recv::Message(42)]);
    assert!(rx.on_completion(&mut io, wake_c).unwrap());
}

#[test]
fn two_phase_begin_finish_close_repeated_stop_and_spurious_wake_in_wait_stop() {
    // Repeated stop() emits at most one Recv::Stop.
    let (tx_s, mut rx_s) = queue_parked::<u32>(2);
    tx_s.stop();
    tx_s.stop();
    assert_eq!(rx_s.try_recv(), TryRecv::Stop);
    assert_eq!(rx_s.try_recv(), TryRecv::Closed);

    let cfg = Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token::new(1),
    };
    let (tx, mut rx) = mailbox_parked::<u32, u32>(cfg);
    let mut w = SlotWaiter::new();
    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(s0)) = w.poll_one(tx.pool(), || !tx.is_closed(), &mut cx) else {
        panic!("slot 0")
    };
    let Poll::Ready(Some(s1)) = w.poll_one(tx.pool(), || !tx.is_closed(), &mut cx) else {
        panic!("slot 1")
    };
    tx.send_slot(s0, None, s0).unwrap();
    tx.send_slot(s1, Some(91), s1).unwrap();
    assert_eq!(tx.detach(s1), None);

    let mut queued = Vec::new();
    rx.begin_close(|item| queued.push(item));
    assert_eq!(rx.answer(s0, 777), None);
    let mut tags = Vec::new();
    rx.finish_close(|| 999, |tag| tags.push(tag));
    assert_eq!(queued, vec![Recv::Message(s0), Recv::Message(s1)]);
    assert_eq!(tags, vec![91]);
    assert_eq!(tx.poll_answer(s0, &mut cx), Poll::Ready(777));

    // Spurious wake() must not make wait_stop() return before stop().
    let (tx2, mut rx2) = queue_parked::<u32>(2);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_c = done.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        tx2.wake();
        std::thread::sleep(Duration::from_millis(15));
        assert!(!done_c.load(std::sync::atomic::Ordering::Acquire));
        tx2.stop();
    });
    rx2.wait_stop();
    done.store(true, std::sync::atomic::Ordering::Release);
    h.join().unwrap();
}
