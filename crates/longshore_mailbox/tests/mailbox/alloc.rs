// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation verification under `alloc_guard::Guard` and `sealed`.

#![cfg(not(longshore_model))]

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::Token;
use longshore_mailbox::{
    block_on, mailbox_parked, queue_parked, Config, Recv, SlotWaiter, TryRecv, TrySendError,
};

#[global_allocator]
static ALLOC: Guard = Guard;

#[test]
fn queue_send_recv_wraparound_stop_close_allocates_nothing() {
    let (tx, mut rx) = queue_parked::<u64>(4);

    sealed(|| {
        for round in 0..25u64 {
            for i in 0..4 {
                assert_eq!(tx.try_send(round * 10 + i), Ok(()));
            }
            assert_eq!(tx.try_send(999), Err(TrySendError::Full(999)));
            for i in 0..4 {
                assert_eq!(rx.try_recv(), TryRecv::Message(round * 10 + i));
            }
            assert_eq!(rx.try_recv(), TryRecv::Empty);
        }

        for i in 0..4 {
            assert_eq!(tx.try_send(100 + i), Ok(()));
        }
        tx.stop();
        assert_eq!(tx.try_send(500), Err(TrySendError::Closed(500)));
        rx.close(|item| match item {
            Recv::Message(_) | Recv::Stop | Recv::Closed => {}
        });
    });
}

#[test]
fn waiters_within_prealloc_and_warmed_block_on_allocate_nothing() {
    let cfg = Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token(1),
    };
    let (tx, mut rx) = mailbox_parked::<(u32, u32), u32>(cfg);

    // Pre-warm thread-local PARKER waker before sealing allocations.
    assert_eq!(block_on(core::future::ready(1u32)), 1);

    let mut cx = Context::from_waker(Waker::noop());
    let mut w0 = SlotWaiter::new();
    let mut w1 = SlotWaiter::new();
    let mut w2 = SlotWaiter::new();
    let mut w3 = SlotWaiter::new();

    sealed(|| {
        // Acquire both slots.
        let Poll::Ready(Some(s0)) = w0.poll_one(tx.pool(), || !tx.is_closed(), &mut cx) else {
            panic!("slot 0");
        };
        let Poll::Ready(Some(s1)) = w1.poll_one(tx.pool(), || !tx.is_closed(), &mut cx) else {
            panic!("slot 1");
        };
        // Queue 2 more waiters (<= waiters_prealloc = 4).
        assert_eq!(
            w2.poll_one(tx.pool(), || !tx.is_closed(), &mut cx),
            Poll::Pending
        );
        assert_eq!(
            w3.poll_one(tx.pool(), || !tx.is_closed(), &mut cx),
            Poll::Pending
        );

        // Send, detach, answer, and reclaim within prealloc.
        tx.send_slot(s0, Some(10), (s0, 1)).unwrap();
        tx.send_slot(s1, None, (s1, 2)).unwrap();
        assert_eq!(tx.detach(s0), None);

        assert_eq!(rx.try_recv(), TryRecv::Message((s0, 1)));
        assert_eq!(rx.try_recv(), TryRecv::Message((s1, 2)));
        assert_eq!(rx.answer(s0, 11), Some(10));
        assert_eq!(rx.answer(s1, 22), None);
        assert_eq!(tx.poll_answer(s1, &mut cx), Poll::Ready(22));

        // Waiting callers take the freed slots without allocating.
        assert_eq!(
            w2.poll_one(tx.pool(), || !tx.is_closed(), &mut cx),
            Poll::Ready(Some(s1))
        );
        assert_eq!(
            w3.poll_one(tx.pool(), || !tx.is_closed(), &mut cx),
            Poll::Ready(Some(s0))
        );
        tx.pool().give(s0);
        tx.pool().give(s1);

        // CallFuture polled + answered + awaited through warmed block_on.
        let mut fut = pin!(tx.call(|slot| (slot, 77), 0));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        let TryRecv::Message((slot, val)) = rx.try_recv() else {
            panic!("expected msg");
        };
        assert_eq!(rx.answer(slot, val + 1), None);
        assert_eq!(block_on(fut), 78);
    });
}
