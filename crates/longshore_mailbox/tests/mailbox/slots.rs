// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Functional tests for `Pool`, `SlotWaiter`, `Cells`, `CallFuture`, and
//! `block_on`.

#![cfg(not(longshore_model))]

use core::future::Future;
use core::pin::pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use longshore_common::io::Token;
use longshore_mailbox::{
    block_on, mailbox_parked, waiter_id, Cells, Config, Pool, Recv, SlotWaiter, TryRecv,
};
use std::sync::Arc;
use std::task::Wake;

struct CountWaker(AtomicUsize);

impl Wake for CountWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn counted_waker() -> (Arc<CountWaker>, Waker) {
    let c = Arc::new(CountWaker(AtomicUsize::new(0)));
    let w = Waker::from(c.clone());
    (c, w)
}

#[test]
fn pool_grows_beyond_waiters_prealloc_and_forgets_pass_on() {
    let pool = Pool::new(1, 2);
    let mut cx_noop = Context::from_waker(Waker::noop());
    let holder = waiter_id();
    assert_eq!(pool.poll_take(holder, &mut cx_noop), Poll::Ready(0));

    // Queue 4 waiters (> waiters_prealloc = 2); none are refused.
    let mut waiters = Vec::new();
    for _ in 0..4 {
        let id = waiter_id();
        let (cnt, w) = counted_waker();
        let mut cx = Context::from_waker(&w);
        assert_eq!(pool.poll_take(id, &mut cx), Poll::Pending);
        waiters.push((id, cnt, w));
    }

    // Returning slot wakes waiter 0; if waiter 0 forgets, wake passes to waiter 1.
    pool.give(0);
    assert_eq!(waiters[0].1 .0.load(Ordering::Relaxed), 1);
    pool.forget(waiters[0].0);
    assert_eq!(waiters[1].1 .0.load(Ordering::Relaxed), 1);

    // Waiters 1..=3 take the slot in FIFO order as each returns it.
    for (id, _, w) in &waiters[1..] {
        let mut cx = Context::from_waker(w);
        assert_eq!(pool.poll_take(*id, &mut cx), Poll::Ready(0));
        pool.give(0);
    }
}

#[test]
fn multi_pool_wait_respects_open_flags_and_take_closed() {
    struct Entry {
        pool: Pool,
        open: bool,
    }
    let mut entries = vec![
        Entry {
            pool: Pool::new(1, 2),
            open: true,
        },
        Entry {
            pool: Pool::new(1, 2),
            open: true,
        },
    ];
    let mut cx = Context::from_waker(Waker::noop());
    let mut w = SlotWaiter::new();

    // Entry 0 closed, entry 1 open -> takes from entry 1.
    entries[0].open = false;
    assert_eq!(
        w.poll_many(&entries, 0, |e| &e.pool, |e| e.open, false, &mut cx),
        Poll::Ready(Some((1, 0)))
    );

    // Entry 1 also closed: take_closed = false returns None immediately.
    entries[1].open = false;
    assert_eq!(
        w.poll_many(&entries, 0, |e| &e.pool, |e| e.open, false, &mut cx),
        Poll::Ready(None)
    );

    // take_closed = true still finds the unused slot in closed entry 0.
    assert_eq!(
        w.poll_many(&entries, 0, |e| &e.pool, |e| e.open, true, &mut cx),
        Poll::Ready(Some((0, 0)))
    );
}

#[test]
fn call_future_await_wait_and_detach_tag_order() {
    let cfg = Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token(1),
    };
    let (tx, mut rx) = mailbox_parked::<(u32, u32), Result<u32, ()>>(cfg);

    // Future path with waker notification.
    let (cnt, w) = counted_waker();
    let mut cx = Context::from_waker(&w);
    let mut fut = pin!(tx.call(|slot| (slot, 41), Err(())));
    assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);

    let TryRecv::Message((slot, val)) = rx.try_recv() else {
        panic!("expected message");
    };
    assert_eq!(rx.answer(slot, Ok(val + 1)), None);
    assert_eq!(cnt.0.load(Ordering::Relaxed), 1);
    assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(42)));

    // Blocking wait() path via worker thread.
    let tx2 = tx.clone();
    let h = std::thread::spawn(move || {
        while let Recv::Message((slot, val)) = rx.recv_parked() {
            if val == 999 {
                let tag = rx.answer(slot, Ok(val));
                assert_eq!(tag, Some(77));
                // Verify slot was already freed into pool before tag returned.
                let mut cx = Context::from_waker(Waker::noop());
                let check_id = waiter_id();
                assert_eq!(rx.pool().poll_take(check_id, &mut cx), Poll::Ready(slot));
                rx.pool().give(slot);
                break;
            }
            assert_eq!(rx.answer(slot, Ok(val * 2)), None);
        }
    });

    assert_eq!(tx2.call(|slot| (slot, 10), Err(())).wait(), Ok(20));

    // Caller attaches tag 77 and detaches before answer.
    let mut waiter = SlotWaiter::new();
    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(slot)) = waiter.poll_one(tx2.pool(), || !tx2.is_closed(), &mut cx) else {
        panic!("slot expected");
    };
    tx2.send_slot(slot, Some(77), (slot, 999)).unwrap();
    assert_eq!(tx2.detach(slot), None);
    h.join().unwrap();
}

#[test]
fn two_phase_close_keeps_existing_answers_and_skips_idle_cells() {
    let pool = Pool::new(4, 4);
    let cells = Cells::<Result<u32, &'static str>>::new(4);

    // Slot 0: sent & answered before close.
    // Slot 1: sent & unanswered waiting caller.
    // Slot 2: idle (never sent).
    // Slot 3: sent & detached with tag 55.
    cells.prepare(0, None);
    cells.prepare(1, None);
    cells.prepare(3, Some(55));
    assert_eq!(cells.answer(&pool, 0, Ok(100)), None);
    assert_eq!(cells.detach(&pool, 3), None);

    let mut cx = Context::from_waker(Waker::noop());
    // Drain slots from pool so only close_unanswered returns detached slot 3.
    for expected in 0..4 {
        assert_eq!(
            pool.poll_take(expected + 1, &mut cx),
            Poll::Ready(expected as u32)
        );
    }

    let mut detached_tags = Vec::new();
    cells.close_unanswered(&pool, || Err("closed"), |tag| detached_tags.push(tag));

    assert_eq!(detached_tags, vec![55]);
    // Detached slot 3 was returned to pool.
    assert_eq!(pool.poll_take(100, &mut cx), Poll::Ready(3));
    assert_eq!(cells.poll_answer(&pool, 0, &mut cx), Poll::Ready(Ok(100)));
    assert_eq!(
        cells.poll_answer(&pool, 1, &mut cx),
        Poll::Ready(Err("closed"))
    );
    // Slot 2 was idle and never answered.
    assert_eq!(cells.poll_answer(&pool, 2, &mut cx), Poll::Pending);

    // block_on smoke test.
    let out = block_on(core::future::ready(777));
    assert_eq!(out, 777);
}

#[test]
fn poll_one_forgets_waiter_when_is_open_flips_false_mid_poll() {
    let pool = Pool::new(1, 2);
    let mut cx_noop = Context::from_waker(Waker::noop());
    let holder = waiter_id();
    assert_eq!(pool.poll_take(holder, &mut cx_noop), Poll::Ready(0));

    let (cnt1, w1) = counted_waker();
    {
        let mut cx1 = Context::from_waker(&w1);
        let mut sw1 = SlotWaiter::new();
        let calls = AtomicUsize::new(0);
        // First is_open() check sees true; after poll_take queues sw1, is_open() flips false.
        assert_eq!(
            sw1.poll_one(
                &pool,
                || calls.fetch_add(1, Ordering::Relaxed) == 0,
                &mut cx1
            ),
            Poll::Ready(None)
        );
    }

    let (cnt2, w2) = counted_waker();
    let mut cx2 = Context::from_waker(&w2);
    let mut sw2 = SlotWaiter::new();
    assert_eq!(sw2.poll_one(&pool, || true, &mut cx2), Poll::Pending);

    pool.give(0);
    assert_eq!(
        cnt1.0.load(Ordering::Relaxed),
        0,
        "closed waiter must be removed from pool.waiters"
    );
    assert_eq!(
        cnt2.0.load(Ordering::Relaxed),
        1,
        "live waiter must be woken when slot is returned"
    );
    assert_eq!(sw2.poll_one(&pool, || true, &mut cx2), Poll::Ready(Some(0)));
}

#[test]
fn poll_many_forgets_closed_entry_and_never_strands_other_waiter() {
    use core::sync::atomic::{AtomicBool, Ordering as AOrd};
    let p0 = Pool::new(1, 2);
    let p1 = Pool::new(1, 2);
    let mut cx_noop = Context::from_waker(Waker::noop());
    assert_eq!(p0.poll_take(1, &mut cx_noop), Poll::Ready(0));
    assert_eq!(p1.poll_take(2, &mut cx_noop), Poll::Ready(0));

    struct Entry {
        pool: Pool,
        open: AtomicBool,
    }
    let entries = [
        Entry {
            pool: p0,
            open: AtomicBool::new(true),
        },
        Entry {
            pool: p1,
            open: AtomicBool::new(true),
        },
    ];
    let (cnt_a, w_a) = counted_waker();
    let (cnt_b, w_b) = counted_waker();
    let mut cx_a = Context::from_waker(&w_a);
    let mut cx_b = Context::from_waker(&w_b);
    let mut waiter_a = SlotWaiter::new();
    let mut waiter_b = SlotWaiter::new();

    assert_eq!(
        waiter_a.poll_many(
            &entries,
            0,
            |e| &e.pool,
            |e| e.open.load(AOrd::Relaxed),
            false,
            &mut cx_a,
        ),
        Poll::Pending
    );
    assert_eq!(
        waiter_b.poll_one(&entries[0].pool, || true, &mut cx_b),
        Poll::Pending
    );

    entries[0].open.store(false, AOrd::Relaxed);
    assert_eq!(
        waiter_a.poll_many(
            &entries,
            0,
            |e| &e.pool,
            |e| e.open.load(AOrd::Relaxed),
            false,
            &mut cx_a,
        ),
        Poll::Pending
    );

    entries[0].pool.give(0);
    assert_eq!(
        cnt_b.0.load(AOrd::Relaxed),
        1,
        "waiter_b must be woken when p0 frees slot"
    );
    assert_eq!(
        cnt_a.0.load(AOrd::Relaxed),
        0,
        "closed waiter_a must not steal p0 wake"
    );
    assert_eq!(
        waiter_b.poll_one(&entries[0].pool, || true, &mut cx_b),
        Poll::Ready(Some(0))
    );
}

#[test]
fn poll_many_empty_first_offset_single_take_closed_mid_flip_and_forget_passes_on() {
    let mut cx_noop = Context::from_waker(Waker::noop());
    let mut w = SlotWaiter::new();
    let empty: [Pool; 0] = [];
    assert_eq!(
        w.poll_many(&empty, 0, |p| p, |_| true, false, &mut cx_noop),
        Poll::Ready(None)
    );

    let single = [Pool::new(1, 2)];
    assert_eq!(
        w.poll_many(&single, 0, |p| p, |_| false, true, &mut cx_noop),
        Poll::Ready(Some((0, 0)))
    );
    single[0].give(0);

    let pools = [Pool::new(1, 2), Pool::new(1, 2)];
    assert_eq!(
        w.poll_many(&pools, 1, |p| p, |_| true, false, &mut cx_noop),
        Poll::Ready(Some((1, 0)))
    );
    let (cnt_other, w_other) = counted_waker();
    let mut cx_other = Context::from_waker(&w_other);
    let mut w2 = SlotWaiter::new();
    let mut w3 = SlotWaiter::new();
    assert_eq!(
        pools[0].poll_take(waiter_id(), &mut cx_noop),
        Poll::Ready(0)
    );
    assert_eq!(
        w2.poll_many(&pools, 1, |p| p, |_| true, false, &mut cx_noop),
        Poll::Pending
    );
    assert_eq!(
        w3.poll_one(&pools[0], || true, &mut cx_other),
        Poll::Pending
    );
    pools[0].give(0);
    w2.forget_many(&pools, |p| p);
    assert_eq!(cnt_other.0.load(Ordering::Relaxed), 1);

    // Mid-loop is_open flip triggers retry for both take_closed=false and take_closed=true.
    for take_closed in [false, true] {
        let fp = [Pool::new(1, 2), Pool::new(1, 2)];
        assert_eq!(fp[0].poll_take(waiter_id(), &mut cx_noop), Poll::Ready(0));
        assert_eq!(fp[1].poll_take(waiter_id(), &mut cx_noop), Poll::Ready(0));
        let calls = AtomicUsize::new(0);
        let mut wf = SlotWaiter::new();
        assert_eq!(
            wf.poll_many(
                &fp,
                0,
                |p| p,
                |_| calls.fetch_add(1, Ordering::Relaxed) < 2,
                take_closed,
                &mut cx_noop,
            ),
            Poll::Ready(None)
        );
    }
}
