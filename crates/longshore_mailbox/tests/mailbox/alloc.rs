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

#[test]
fn poll_many_begin_finish_close_and_dropped_call_future_allocate_nothing() {
    use longshore_common::io::{Completions, Io, Limits};
    use longshore_common::sim::{Faults, SimIo};
    use longshore_mailbox::mailbox;

    let limits = Limits {
        ops_max: 4,
        ..Limits::BASE
    };
    let mut sim_a = SimIo::new(limits, 1, Faults::default());
    let mut sim_b = SimIo::new(limits, 2, Faults::default());
    let cfg = Config {
        capacity: 4,
        slots: 1,
        waiters_prealloc: 4,
        wake_token: Token(9),
    };
    let (tx_a, mut rx_a) = mailbox::<u32, u32, _>(&mut sim_a, cfg).unwrap();
    let (tx_b, mut rx_b) = mailbox::<u32, u32, _>(&mut sim_b, cfg).unwrap();
    rx_a.arm(&mut sim_a).unwrap();
    rx_b.arm(&mut sim_b).unwrap();
    let mut out_a = Completions::new(8);
    let mut cx = Context::from_waker(Waker::noop());
    let mut w_many = SlotWaiter::new();

    sealed(|| {
        let Poll::Ready(Some(sa)) =
            SlotWaiter::new().poll_one(tx_a.pool(), || !tx_a.is_closed(), &mut cx)
        else {
            panic!("slot a")
        };
        let Poll::Ready(Some((1, sb))) = w_many.poll_many(
            &[&tx_a, &tx_b],
            0,
            |s| s.pool(),
            |s| !s.is_closed(),
            false,
            &mut cx,
        ) else {
            panic!("slot b")
        };
        tx_b.pool().give(sb);

        let Poll::Ready(Some(sb2)) =
            SlotWaiter::new().poll_one(tx_b.pool(), || !tx_b.is_closed(), &mut cx)
        else {
            panic!("slot b2")
        };
        assert_eq!(
            w_many.poll_many(
                &[&tx_a, &tx_b],
                0,
                |s| s.pool(),
                |s| !s.is_closed(),
                false,
                &mut cx,
            ),
            Poll::Pending
        );
        w_many.forget_many(&[&tx_a, &tx_b], |s| s.pool());

        // CallFuture dropped in Stage::Slot (queued in Pool::waiters).
        {
            let mut fut_slot = pin!(tx_a.call(|s| s, 0));
            assert_eq!(fut_slot.as_mut().poll(&mut cx), Poll::Pending);
        }
        tx_a.pool().give(sa);
        tx_b.pool().give(sb2);

        // CallFuture dropped in Stage::Sent (detach) then answered under sealed.
        {
            let mut fut = pin!(tx_a.call(|s| s, 0));
            assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        }
        let _ = sim_a.tick(false, &mut out_a);
        while let Some(c) = out_a.pop() {
            let _ = rx_a.on_completion(&mut sim_a, c);
        }
        let mut got_slot = None;
        rx_a.drain(|item| {
            if let Recv::Message(s) = item {
                got_slot = Some(s);
            }
        });
        if let Some(s) = got_slot {
            let _ = rx_a.answer(s, 1);
        }
        rx_a.begin_close(|_| {});
        rx_a.finish_close(|| 0, |_| {});
        rx_b.close(|_| {}, || 0, |_| {});
    });
}
