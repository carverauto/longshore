// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 25 E2E tests: generic `Pool`, slots, multi-pool wait, `Future` & `block_on`.

use super::super::harness::HttpStoreServer;
use longshore::client::Builder;
use longshore::io::alloc_guard::sealed;
use longshore::io::Token;
use longshore::mailbox::{
    block_on, mailbox_parked, queue_parked, Cells, Config, Pool, Recv, SlotWaiter, TryRecv,
};
use longshore::store::creds::Credentials;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

struct CountWake(AtomicUsize);
impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn counting_waker() -> (Arc<CountWake>, Waker) {
    let c = Arc::new(CountWake(AtomicUsize::new(0)));
    let w = Waker::from(Arc::clone(&c));
    (c, w)
}

#[test]
fn t1_f25_1_blocking_caller_sends_request_and_receives_answer() {
    let (tx, mut rx) = mailbox_parked::<u32, u64>(Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token(1),
    });

    let worker = thread::spawn(move || {
        if let Recv::Message(slot) = rx.recv_parked() {
            assert!(rx.answer(slot, 42_000 + slot as u64).is_none());
        }
    });

    let ans = tx.call(|slot| slot, 0).wait();
    assert_eq!(ans, 42_000);
    worker.join().unwrap();
}

#[test]
fn t1_f25_2_future_caller_woken_and_yields_answer_via_block_on() {
    let (tx, mut rx) = mailbox_parked::<u32, &'static str>(Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token(1),
    });

    let (wakes, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(tx.call(|slot| slot, "closed"));
    assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

    let slot = match rx.try_recv() {
        TryRecv::Message(s) => s,
        other @ (TryRecv::Stop | TryRecv::Empty | TryRecv::Closed) => {
            panic!("expected message, got {other:?}")
        }
    };
    assert!(rx.answer(slot, "pong").is_none());
    assert!(wakes.0.load(Ordering::Relaxed) >= 1);
    assert_eq!(block_on(fut), "pong");
}

#[test]
fn t1_f25_3_multi_pool_wait_selects_free_pool_immediately() {
    let p0 = Pool::new(1, 2);
    let p1 = Pool::new(1, 2);
    let p2 = Pool::new(1, 2);
    let c0 = Cells::<u32>::new(1);
    let c1 = Cells::<u32>::new(1);

    // Occupy p0's single slot.
    let (_cnt, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    let mut w0 = SlotWaiter::new();
    let Poll::Ready(Some(s0)) = w0.poll_one(&p0, || true, &mut cx) else {
        panic!("p0 slot 0 should be free");
    };

    // Wait across [p0, p1, p2] -> must immediately obtain slot from p1 (index 1).
    let pools = [&p0, &p1, &p2];
    let mut w1 = SlotWaiter::new();
    let Poll::Ready(Some((which, s1))) = w1.poll_many(&pools, 0, |p| *p, |_| true, false, &mut cx)
    else {
        panic!("p1 should satisfy poll_many");
    };
    assert_eq!(which, 1);

    c0.prepare(s0, None);
    c1.prepare(s1, None);
    assert!(c0.answer(&p0, s0, 10).is_none());
    assert!(c1.answer(&p1, s1, 20).is_none());
    assert_eq!(c0.wait_answer(&p0, s0), 10);
    assert_eq!(c1.wait_answer(&p1, s1), 20);
}

#[test]
fn t1_f25_4_waiting_within_preallocated_waiters_allocates_zero_heap() {
    let pool = Pool::new(2, 8);
    let cells = Cells::<u32>::new(2);
    let (_c, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);

    let ok = sealed(|| {
        let Poll::Ready(s0) = pool.poll_take(10, &mut cx) else {
            return false;
        };
        let Poll::Ready(s1) = pool.poll_take(11, &mut cx) else {
            return false;
        };
        for wid in 12..18u64 {
            if !matches!(pool.poll_take(wid, &mut cx), Poll::Pending) {
                return false;
            }
        }
        for wid in 12..18u64 {
            pool.forget(wid);
        }
        cells.prepare(s0, None);
        cells.prepare(s1, None);
        cells.answer(&pool, s0, 1);
        cells.answer(&pool, s1, 2);
        matches!(cells.poll_answer(&pool, s0, &mut cx), Poll::Ready(1))
            && matches!(cells.poll_answer(&pool, s1, &mut cx), Poll::Ready(2))
    });
    assert!(ok);
}

#[test]
fn t1_f25_5_mailbox_block_on_drives_both_mailbox_and_s3_client_pending() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let mut b = Builder::new(
        "127.0.0.1",
        "us-east-1",
        Credentials::new("K", "S").unwrap(),
    );
    b.tls = false;
    b.port = srv.port();
    b.address = Some(srv.addr());
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.buffers = 2;
    b.buffer_size = 16 * 1024;
    b.connections = 2;
    b.operations = 2;
    let client = b.build().expect("client");

    let buf = block_on(client.buffer());
    let (res, buf) = block_on(client.store("block_on_key", buf, 0));
    res.expect("store via mailbox::block_on");
    drop(buf);
}

#[test]
fn t2_f25_1_dropped_future_frees_slot_first_and_returns_attached_tag() {
    let (tx, mut rx) = mailbox_parked::<u32, u64>(Config {
        capacity: 4,
        slots: 1,
        waiters_prealloc: 2,
        wake_token: Token(1),
    });
    let (_c, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);

    let mut waiter = SlotWaiter::new();
    let Poll::Ready(Some(s0)) = waiter.poll_one(tx.pool(), || !tx.is_closed(), &mut cx) else {
        panic!("expected free slot");
    };
    tx.send_slot(s0, Some(42), s0).unwrap();
    assert_eq!(tx.detach(s0), None);

    let slot = match rx.try_recv() {
        TryRecv::Message(s) => s,
        other @ (TryRecv::Stop | TryRecv::Empty | TryRecv::Closed) => {
            panic!("expected slot, got {other:?}")
        }
    };
    let returned_tag = rx.answer(slot, 9999);
    assert_eq!(returned_tag, Some(42), "detached tag 42 must come back");

    // Next caller must immediately acquire the freed slot and get its own fresh answer.
    let mut next = pin!(tx.call(|s| s, 0));
    assert!(matches!(next.as_mut().poll(&mut cx), Poll::Pending));
    let slot2 = match rx.try_recv() {
        TryRecv::Message(s) => s,
        other @ (TryRecv::Stop | TryRecv::Empty | TryRecv::Closed) => {
            panic!("expected slot2, got {other:?}")
        }
    };
    assert!(rx.answer(slot2, 555).is_none());
    assert_eq!(block_on(next), 555);
}

#[test]
fn t2_f25_2_waiters_exceeding_slots_and_preallocated_grow_and_complete() {
    let (tx, mut rx) = mailbox_parked::<u32, u32>(Config {
        capacity: 8,
        slots: 1,
        waiters_prealloc: 2,
        wake_token: Token(1),
    });

    let receiver = thread::spawn(move || {
        for _ in 0..5 {
            let Recv::Message(slot) = rx.recv_parked() else {
                panic!("expected msg");
            };
            thread::sleep(Duration::from_millis(2));
            assert!(rx.answer(slot, 100 + slot).is_none());
        }
    });

    let mut callers = Vec::new();
    for _ in 0..5 {
        let t = tx.clone();
        callers.push(thread::spawn(move || t.call(|s| s, 0).wait()));
    }
    for h in callers {
        assert_eq!(h.join().unwrap(), 100);
    }
    receiver.join().unwrap();
}

#[test]
fn t2_f25_3_close_delivers_answered_slot_and_closed_to_unanswered() {
    let (tx, mut rx) = mailbox_parked::<u32, u32>(Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token(1),
    });
    let (_c, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);

    let mut f0 = pin!(tx.call(|s| s, u32::MAX));
    let mut f1 = pin!(tx.call(|s| s, u32::MAX));
    assert!(matches!(f0.as_mut().poll(&mut cx), Poll::Pending));
    assert!(matches!(f1.as_mut().poll(&mut cx), Poll::Pending));

    let TryRecv::Message(s0) = rx.try_recv() else {
        panic!("expected s0");
    };
    let TryRecv::Message(_s1) = rx.try_recv() else {
        panic!("expected s1");
    };
    assert!(rx.answer(s0, 111).is_none());
    rx.close(|_| {}, || u32::MAX, |_| {});

    assert_eq!(block_on(f0), 111);
    assert_eq!(block_on(f1), u32::MAX);
}

#[test]
fn t2_f25_4_close_never_overwrites_unsent_free_slots() {
    let pool = Pool::new(3, 4);
    let cells = Cells::<u32>::new(3);
    let (_c, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    let Poll::Ready(s0) = pool.poll_take(1, &mut cx) else {
        panic!("slot 0 free");
    };
    cells.prepare(s0, None);
    cells.answer(&pool, s0, 77);
    // Slots 1 and 2 are free/unsent; close_unanswered must not panic or fabricate answers.
    cells.close_unanswered(&pool, || 999, |_| {});
    assert_eq!(cells.poll_answer(&pool, s0, &mut cx), Poll::Ready(77));
    assert_eq!(cells.poll_answer(&pool, 1, &mut cx), Poll::Pending);
    assert_eq!(cells.poll_answer(&pool, 2, &mut cx), Poll::Pending);
}

#[test]
#[should_panic]
fn t2_f25_5_zero_capacity_pool_rejected_and_wake_without_send_is_safe() {
    let (tx, mut rx) = queue_parked::<u32>(2);
    tx.wake();
    assert_eq!(rx.try_recv(), TryRecv::Empty);
    let _: Pool = Pool::new(0, 1);
}
