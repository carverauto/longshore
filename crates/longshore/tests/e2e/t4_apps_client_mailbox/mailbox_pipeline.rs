// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-10`: Cross-core mailbox service pipeline (2 `Posix` receiver threads,
//! 4 mixed blocking/async callers, multi-pool dispatch, caller detach `u32` tag,
//! `Sender::stop()` drain, and receiver `close()` race).

use super::super::harness::posix_backend;
use longshore::io::{Completions, Io, Limits, Token};
use longshore::mailbox::{block_on, mailbox, Config, Sender, SlotWaiter, TryRecv, TrySendError};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::thread;

#[derive(Clone, Copy, Debug)]
struct Job {
    val: u32,
    slot: u32,
}

fn spawn_posix_receiver(
    tx_done: Arc<AtomicUsize>,
    detached_tags: Arc<std::sync::Mutex<Vec<u32>>>,
    close_after_n: Option<usize>,
    answer_gate: Option<Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
) -> (Sender<Job, u32>, thread::JoinHandle<()>) {
    let lim = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    };
    let (tx_ready, rx_ready) = std::sync::mpsc::sync_channel(1);

    let handle = thread::spawn(move || {
        let mut io = posix_backend(lim).unwrap();
        let (tx, mut io_rx) = mailbox::<Job, u32, _>(
            &mut io,
            Config {
                capacity: 16,
                slots: 4,
                waiters_prealloc: 16,
                wake_token: Token(1),
            },
        )
        .unwrap();
        tx_ready.send(tx).unwrap();
        let mut out = Completions::new(8);
        let mut handled = 0usize;
        io_rx.arm(&mut io).unwrap();

        loop {
            io.tick(true, &mut out).unwrap();
            let mut saw_stop = false;
            while let Some(c) = out.pop() {
                if io_rx.on_completion(&mut io, c).unwrap() {
                    loop {
                        match io_rx.try_recv() {
                            TryRecv::Message(job) => {
                                handled += 1;
                                tx_done.fetch_add(1, Ordering::Relaxed);
                                if handled == 1 {
                                    if let Some(gate) = &answer_gate {
                                        let (lock, cv) = &**gate;
                                        let mut ready = lock.lock().unwrap();
                                        while !*ready {
                                            ready = cv.wait(ready).unwrap();
                                        }
                                    }
                                }
                                if let Some(tag) = io_rx.answer(job.slot, job.val * 2) {
                                    detached_tags.lock().unwrap().push(tag);
                                }
                                if close_after_n == Some(handled) {
                                    io_rx.close(
                                        |_| {},
                                        || u32::MAX,
                                        |tag| detached_tags.lock().unwrap().push(tag),
                                    );
                                    return;
                                }
                            }
                            TryRecv::Stop | TryRecv::Closed => {
                                saw_stop = true;
                                break;
                            }
                            TryRecv::Empty => break,
                        }
                    }
                }
            }
            if saw_stop {
                io_rx.close(
                    |_| {},
                    || u32::MAX,
                    |tag| detached_tags.lock().unwrap().push(tag),
                );
                break;
            }
        }
    });

    (rx_ready.recv().unwrap(), handle)
}

#[cfg(unix)]
#[test]
fn t4_10_cross_core_mailbox_pipeline_multipool_stop_and_close_race() {
    // Phase A: 2 Posix receiver threads, multi-pool selection, blocking + Future callers,
    // caller cancellation returning u32 tag, and graceful Sender::stop().
    let handled_a = Arc::new(AtomicUsize::new(0));
    let tags_a = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate0 = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let gate1 = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (tx0, h0) = spawn_posix_receiver(
        Arc::clone(&handled_a),
        Arc::clone(&tags_a),
        None,
        Some(Arc::clone(&gate0)),
    );
    let (tx1, h1) = spawn_posix_receiver(
        Arc::clone(&handled_a),
        Arc::clone(&tags_a),
        None,
        Some(Arc::clone(&gate1)),
    );

    // Test detach returning caller-attached u32 tag (0xBEEF) to receiver after freeing slot.
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut w_detach = SlotWaiter::new();
    let Poll::Ready(Some(slot0)) = w_detach.poll_one(tx0.pool(), || !tx0.is_closed(), &mut cx)
    else {
        panic!("tx0 slot should be free");
    };
    tx0.send_slot(
        slot0,
        Some(0xBEEF),
        Job {
            val: 7,
            slot: slot0,
        },
    )
    .unwrap();
    assert_eq!(tx0.detach(slot0), None);
    {
        let (lock, cv) = &*gate0;
        *lock.lock().unwrap() = true;
        cv.notify_one();
    }

    // Multi-pool wait across [&tx0, &tx1] via SlotWaiter::poll_many.
    let mut held = Vec::with_capacity(4);
    let mut w_hold = SlotWaiter::new();
    while held.len() < 4 {
        let slot = block_on(core::future::poll_fn(|cx| {
            w_hold.poll_one(tx0.pool(), || !tx0.is_closed(), cx)
        }))
        .expect("tx0 pool should yield all 4 slots");
        held.push(slot);
    }
    let mut w_many = SlotWaiter::new();
    let Poll::Ready(Some((which, slot1))) = w_many.poll_many(
        &[&tx0, &tx1],
        0,
        |s| s.pool(),
        |s| !s.is_closed(),
        false,
        &mut cx,
    ) else {
        panic!("tx1 should satisfy poll_many while tx0 is full");
    };
    assert_eq!(which, 1);
    tx1.send_slot(
        slot1,
        Some(0xCAFE),
        Job {
            val: 9,
            slot: slot1,
        },
    )
    .unwrap();
    assert_eq!(tx1.detach(slot1), None);
    {
        let (lock, cv) = &*gate1;
        *lock.lock().unwrap() = true;
        cv.notify_one();
    }
    for s in held {
        tx0.pool().give(s);
    }

    // 4 worker threads mixing blocking wait and Future + block_on across both receivers.
    let mut workers = Vec::new();
    for wid in 0..4u32 {
        let t0 = tx0.clone();
        let t1 = tx1.clone();
        workers.push(thread::spawn(move || {
            for iter in 0..8u32 {
                let val = wid * 100 + iter;
                if iter % 2 == 0 {
                    let ans = t0.call(|slot| Job { val, slot }, u32::MAX).wait();
                    assert_eq!(ans, val * 2);
                } else {
                    let ans = block_on(t1.call(|slot| Job { val, slot }, u32::MAX));
                    assert_eq!(ans, val * 2);
                }
            }
        }));
    }
    for w in workers {
        w.join().unwrap();
    }

    // Graceful stop: queued items drain before stop, subsequent sends get Closed.
    tx0.stop();
    tx1.stop();
    h0.join().unwrap();
    h1.join().unwrap();
    assert!(matches!(
        tx0.try_send(Job { val: 1, slot: 0 }),
        Err(TrySendError::Closed(_))
    ));
    let tags = tags_a.lock().unwrap();
    assert!(tags.contains(&0xBEEF));
    assert!(tags.contains(&0xCAFE));

    // Phase B: Receiver close() racing active senders — every message either answered or Closed.
    let handled_b = Arc::new(AtomicUsize::new(0));
    let tags_b = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (tx_race, h_race) = spawn_posix_receiver(Arc::clone(&handled_b), tags_b, Some(5), None);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let mut racers = Vec::new();
    let answered_cnt = Arc::new(AtomicUsize::new(0));
    let closed_cnt = Arc::new(AtomicUsize::new(0));

    for rid in 0..3u32 {
        let tx_r = tx_race.clone();
        let sf = Arc::clone(&stop_flag);
        let ac = Arc::clone(&answered_cnt);
        let cc = Arc::clone(&closed_cnt);
        racers.push(thread::spawn(move || {
            for k in 0..10u32 {
                if sf.load(Ordering::Relaxed) {
                    break;
                }
                let ans = tx_r
                    .call(
                        |slot| Job {
                            val: rid * 10 + k,
                            slot,
                        },
                        u32::MAX,
                    )
                    .wait();
                if ans == u32::MAX {
                    cc.fetch_add(1, Ordering::Relaxed);
                    sf.store(true, Ordering::Relaxed);
                } else {
                    assert_eq!(ans, (rid * 10 + k) * 2);
                    ac.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    h_race.join().unwrap();
    for r in racers {
        r.join().unwrap();
    }
    assert_eq!(answered_cnt.load(Ordering::Relaxed), 5);
    assert!(closed_cnt.load(Ordering::Relaxed) >= 1);
}
