// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A call that holds a slot but finds the queue full of other messages.

use core::future::Future;
use core::pin::pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use longshore_common::io::Token;
use longshore_mailbox::{mailbox_parked, waiter_id, Config, TryRecv};
use std::sync::Arc;
use std::task::Wake;

struct CountWaker(AtomicUsize);

impl Wake for CountWaker {
    fn wake(self: Arc<Self>) {
        let _: usize = self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _: usize = self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn counted_waker() -> (Arc<CountWaker>, Waker) {
    let c = Arc::new(CountWaker(AtomicUsize::new(0)));
    let w = Waker::from(c.clone());
    (c, w)
}

#[test]
fn a_call_waits_for_room_when_other_messages_fill_the_queue() {
    let cfg = Config::new(1, 2, 2, Token::new(1));
    let (tx, mut rx) = mailbox_parked::<(u32, u32), Result<u32, ()>>(cfg);
    let (cnt, w) = counted_waker();
    let mut cx = Context::from_waker(&w);

    // A message without a slot fills the queue before the call sends.
    assert!(tx.try_send((u32::MAX, 0)).is_ok());
    {
        let mut fut = pin!(tx.call(|slot| (slot, 41), Err(())));
        // It holds a slot and finds the queue full: it asks to be polled again.
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        assert!(cnt.0.load(Ordering::Relaxed) >= 1, "the future woke itself");
        assert!(matches!(rx.try_recv(), TryRecv::Message((u32::MAX, 0))));
        // Room now: this poll sends the call.
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        let TryRecv::Message((slot, val)) = rx.try_recv() else {
            panic!("expected the call's message");
        };
        assert_eq!(rx.answer(slot, Ok(val + 1)), None);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(42)));
    }

    // Dropped while still waiting for room, the call gives its slot back.
    assert!(tx.try_send((u32::MAX, 0)).is_ok());
    {
        let mut fut = pin!(tx.call(|slot| (slot, 7), Err(())));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
    }
    let mut check = Context::from_waker(Waker::noop());
    let (a, b) = (waiter_id(), waiter_id());
    assert!(matches!(tx.pool().poll_take(a, &mut check), Poll::Ready(_)));
    assert!(matches!(tx.pool().poll_take(b, &mut check), Poll::Ready(_)));
}
