// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `loom` model tests for `Pool`, `SlotWaiter`, `Cells`, `CallFuture`, and
//! `detach` / closing answer order.

#![cfg(longshore_model)]

use super::model;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use longshore_common::io::Token;
use longshore_mailbox::{block_on, mailbox_parked, Config, Recv, SlotWaiter};

#[test]
fn slots_two_waiters_on_one_slot() {
    model(2, || {
        let cfg = Config {
            capacity: 1,
            slots: 1,
            waiters_prealloc: 2,
            wake_token: Token(1),
        };
        let (tx1, mut rx) = mailbox_parked::<u32, u32>(cfg);
        let tx2 = tx1.clone();

        let h1 = loom::thread::spawn(move || tx1.call(|s| s + 1, 0).wait());
        let h2 = loom::thread::spawn(move || tx2.call(|s| s + 10, 0).wait());

        while let Recv::Message(v) = rx.recv_parked() {
            assert_eq!(rx.answer(0, v * 2), None);
        }

        assert_eq!(h1.join().unwrap(), 2);
        assert_eq!(h2.join().unwrap(), 20);
    });
}

#[test]
fn slots_future_and_blocking_caller() {
    model(2, || {
        let cfg = Config {
            capacity: 1,
            slots: 1,
            waiters_prealloc: 2,
            wake_token: Token(1),
        };
        let (tx1, mut rx) = mailbox_parked::<u32, u32>(cfg);
        let tx2 = tx1.clone();

        let h1 = loom::thread::spawn(move || block_on(tx1.call(|_| 5, 0)));
        let h2 = loom::thread::spawn(move || tx2.call(|_| 6, 0).wait());

        while let Recv::Message(v) = rx.recv_parked() {
            assert_eq!(rx.answer(0, v + 100), None);
        }

        assert_eq!(h1.join().unwrap(), 105);
        assert_eq!(h2.join().unwrap(), 106);
    });
}

#[test]
fn slots_detach_frees_slot_before_returning_tag() {
    model(2, || {
        let cfg = Config {
            capacity: 1,
            slots: 1,
            waiters_prealloc: 2,
            wake_token: Token(1),
        };
        let (tx1, mut rx) = mailbox_parked::<(u32, u32), u32>(cfg);
        let tx2 = tx1.clone();

        // Caller 1 acquires slot 0, sends with tag 77, and detaches.
        let mut w1 = SlotWaiter::new();
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(s0)) = w1.poll_one(tx1.pool(), || !tx1.is_closed(), &mut cx) else {
            panic!("slot 0");
        };
        tx1.send_slot(s0, Some(77), (s0, 1)).unwrap();
        assert_eq!(tx1.detach(s0), None);

        // Caller 2 waits on the single slot; it can only get slot 0 after receiver frees it.
        let h = loom::thread::spawn(move || {
            let ans = tx2.call(|s| (s, 2), 999).wait();
            assert_eq!(ans, 200);
        });

        let Recv::Message((slot0, _)) = rx.recv_parked() else {
            panic!("msg 1");
        };
        assert_eq!(rx.answer(slot0, 100), Some(77));

        let Recv::Message((slot1, _)) = rx.recv_parked() else {
            panic!("msg 2");
        };
        assert_eq!(rx.answer(slot1, 200), None);
        drop(tx1);
        assert_eq!(rx.recv_parked(), Recv::Closed);
        h.join().unwrap();

        // Single-waiter check: pool.give(slot) has already executed by the time
        // answer() returns Some(tag).
        let (tx, mut rx) = mailbox_parked::<u32, u32>(cfg);
        let Poll::Ready(Some(s)) = w1.poll_one(tx.pool(), || !tx.is_closed(), &mut cx) else {
            panic!("slot");
        };
        tx.send_slot(s, Some(88), s).unwrap();
        assert_eq!(tx.detach(s), None);
        let Recv::Message(s_rx) = rx.recv_parked() else {
            panic!("msg");
        };
        assert_eq!(rx.answer(s_rx, 1), Some(88));
        assert_eq!(
            rx.pool().poll_take(999, &mut cx),
            Poll::Ready(s_rx),
            "slot must be returned to pool before tag is returned"
        );
    });
}

#[test]
fn slots_close_answers_first_then_unanswered_without_overwriting() {
    model(1, || {
        let cfg = Config {
            capacity: 4,
            slots: 4,
            waiters_prealloc: 4,
            wake_token: Token(1),
        };
        let (tx1, mut rx) = mailbox_parked::<(u32, u32), Result<u32, ()>>(cfg);
        let tx2 = tx1.clone();

        // Slot 0: sent & answered BEFORE close; uncollected answer preserved.
        let mut w0 = SlotWaiter::new();
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(s0)) = w0.poll_one(tx1.pool(), || !tx1.is_closed(), &mut cx) else {
            panic!("slot 0");
        };
        tx1.send_slot(s0, None, (s0, 10)).unwrap();
        let Recv::Message((slot0, val)) = rx.recv_parked() else {
            panic!("msg 0");
        };
        assert_eq!(rx.answer(slot0, Ok(val)), None);

        // Slot 2 (taken as next free slot): sent with tag 55 & detached before close.
        let mut w2 = SlotWaiter::new();
        let Poll::Ready(Some(s2)) = w2.poll_one(tx1.pool(), || !tx1.is_closed(), &mut cx) else {
            panic!("slot 2");
        };
        tx1.send_slot(s2, Some(55), (s2, 30)).unwrap();
        assert_eq!(tx1.detach(s2), None);
        assert!(matches!(rx.recv_parked(), Recv::Message(_)));

        let h = loom::thread::spawn(move || {
            let mut f = pin!(tx2.call(|s| (s, 20), Err(())));
            let mut cx = Context::from_waker(Waker::noop());
            if let Poll::Ready(res) = f.as_mut().poll(&mut cx) {
                return res;
            }
            block_on(f)
        });

        let mut detached_tags = Vec::new();
        rx.close(|_| {}, || Err(()), |tag| detached_tags.push(tag));
        assert_eq!(h.join().unwrap(), Err(()));
        assert_eq!(detached_tags, vec![55]);
        // Before collecting slot0, detached s2 (plus idle/h's slot) are all in pool.
        let mut freed = Vec::new();
        for i in 0..3 {
            let Poll::Ready(Some(s)) = SlotWaiter::new().poll_one(tx1.pool(), || true, &mut cx)
            else {
                panic!("expected free slot #{i}");
            };
            assert_ne!(s, slot0);
            freed.push(s);
        }
        assert!(freed.contains(&s2));
        for s in freed {
            tx1.pool().give(s);
        }
        assert_eq!(
            block_on(core::future::poll_fn(|c| tx1.poll_answer(slot0, c))),
            Ok(10)
        );
        // Idle slot 3 was never sent, so it remains unanswered.
        assert_eq!(tx1.poll_answer(3, &mut cx), Poll::Pending);
    });
}
