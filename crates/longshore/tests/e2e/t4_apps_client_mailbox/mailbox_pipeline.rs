// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-10`: Cross-core mailbox service pipeline (2 `Posix` receiver threads,
//! 4 mixed blocking/async callers, multi-pool dispatch, caller detach `u32` tag,
//! `Sender::stop()` drain, and receiver `close()` race).

use super::super::harness::posix_backend;
use longshore::io::{Completions, Io, Limits, Token};
use longshore::mailbox::{
    block_on, mailbox, Config, Receiver, Sender, SlotWaiter, TryRecv, TrySendError,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;

#[derive(Clone, Copy, Debug)]
struct Job {
    val: u32,
    slot: u32,
}

type Gate = Arc<(Mutex<bool>, Condvar)>;
type Tags = Arc<Mutex<Vec<u32>>>;

/// Blocks until `gate` is opened.
fn wait_gate(gate: &Gate) {
    let (lock, cv) = &**gate;
    let mut ready = lock.lock().unwrap();
    while !*ready {
        ready = cv.wait(ready).unwrap();
    }
}

/// Opens `gate`, releasing a receiver parked on it.
fn open_gate(gate: &Gate) {
    let (lock, cv) = &**gate;
    *lock.lock().unwrap() = true;
    cv.notify_one();
}

/// What draining the receiver's queue ended on.
enum Drained {
    Empty,
    Stopped,
    ClosedEarly,
}

/// The receiver thread's bookkeeping and test knobs.
struct ReceiverState {
    tx_done: Arc<AtomicUsize>,
    detached_tags: Tags,
    close_after_n: Option<usize>,
    answer_gate: Option<Gate>,
    handled: usize,
}

impl ReceiverState {
    /// Closes the receiver, recording each detached caller's tag.
    fn close(&self, io_rx: &mut Receiver<Job, u32>) {
        io_rx.close(
            |_| {},
            || u32::MAX,
            |tag| self.detached_tags.lock().unwrap().push(tag),
        );
    }

    /// Answers every queued job with `val * 2` (the first one only once the
    /// gate opens), closing early after `close_after_n` jobs.
    fn drain(&mut self, io_rx: &mut Receiver<Job, u32>) -> Drained {
        loop {
            match io_rx.try_recv() {
                TryRecv::Message(job) => {
                    self.handled += 1;
                    let _: usize = self.tx_done.fetch_add(1, Ordering::Relaxed);
                    if self.handled == 1 {
                        if let Some(gate) = &self.answer_gate {
                            wait_gate(gate);
                        }
                    }
                    if let Some(tag) = io_rx.answer(job.slot, job.val * 2) {
                        self.detached_tags.lock().unwrap().push(tag);
                    }
                    if self.close_after_n == Some(self.handled) {
                        self.close(io_rx);
                        return Drained::ClosedEarly;
                    }
                }
                TryRecv::Stop | TryRecv::Closed => return Drained::Stopped,
                TryRecv::Empty => return Drained::Empty,
            }
        }
    }
}

fn spawn_posix_receiver(
    tx_done: Arc<AtomicUsize>,
    detached_tags: Tags,
    close_after_n: Option<usize>,
    answer_gate: Option<Gate>,
) -> (Sender<Job, u32>, thread::JoinHandle<()>) {
    let lim = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(256)
        .with_ops_max(8);
    let (tx_ready, rx_ready) = std::sync::mpsc::sync_channel(1);
    let mut state = ReceiverState {
        tx_done,
        detached_tags,
        close_after_n,
        answer_gate,
        handled: 0,
    };

    let handle = thread::spawn(move || {
        let mut io = posix_backend(lim).unwrap();
        let (tx, mut io_rx) = mailbox::<Job, u32, _>(
            &mut io,
            Config {
                capacity: 16,
                slots: 4,
                waiters_prealloc: 16,
                wake_token: Token::new(1),
            },
        )
        .unwrap();
        tx_ready.send(tx).unwrap();
        let mut out = Completions::new(8);
        io_rx.arm(&mut io).unwrap();

        loop {
            io.tick(true, &mut out).unwrap();
            let mut saw_stop = false;
            while let Some(c) = out.pop() {
                if io_rx.on_completion(&mut io, c).unwrap() {
                    match state.drain(&mut io_rx) {
                        Drained::Empty => {}
                        Drained::Stopped => saw_stop = true,
                        Drained::ClosedEarly => return,
                    }
                }
            }
            if saw_stop {
                state.close(&mut io_rx);
                break;
            }
        }
    });

    (rx_ready.recv().unwrap(), handle)
}

/// Sends `val` in `slot` with caller tag `tag`, detaches the caller at once,
/// then opens `gate` so the receiver answers into the detached slot.
fn send_detached(tx: &Sender<Job, u32>, slot: u32, tag: u32, val: u32, gate: &Gate) {
    tx.send_slot(slot, Some(tag), Job { val, slot }).unwrap();
    assert_eq!(tx.detach(slot), None);
    open_gate(gate);
}

/// Takes every slot of `tx`'s pool through blocking waits.
fn hold_all_slots(tx: &Sender<Job, u32>) -> Vec<u32> {
    let mut held = Vec::with_capacity(4);
    let mut w_hold = SlotWaiter::new();
    while held.len() < 4 {
        let slot = block_on(core::future::poll_fn(|poll_cx| {
            w_hold.poll_one(tx.pool(), || !tx.is_closed(), poll_cx)
        }))
        .expect("tx0 pool should yield all 4 slots");
        held.push(slot);
    }
    held
}

/// Four workers alternate blocking `wait` calls on `tx0` with `block_on`
/// futures on `tx1`, each checking its doubled answer.
fn run_mixed_callers(tx0: &Sender<Job, u32>, tx1: &Sender<Job, u32>) {
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
}

/// Phase A: 2 Posix receiver threads, multi-pool selection, blocking + Future
/// callers, caller cancellation returning u32 tag, and graceful `Sender::stop()`.
fn multipool_detach_and_stop() {
    let handled_a = Arc::new(AtomicUsize::new(0));
    let tags_a = Arc::new(Mutex::new(Vec::new()));
    let gate0 = Arc::new((Mutex::new(false), Condvar::new()));
    let gate1 = Arc::new((Mutex::new(false), Condvar::new()));
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
    send_detached(&tx0, slot0, 0xBEEF, 7, &gate0);

    // Multi-pool wait across [&tx0, &tx1] via SlotWaiter::poll_many.
    let held = hold_all_slots(&tx0);
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
    send_detached(&tx1, slot1, 0xCAFE, 9, &gate1);
    for s in held {
        tx0.pool().give(s);
    }

    run_mixed_callers(&tx0, &tx1);

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
}

/// Phase B: receiver `close()` racing active senders; every message is
/// either answered or `Closed`.
fn receiver_close_races_senders() {
    let handled_b = Arc::new(AtomicUsize::new(0));
    let tags_b = Arc::new(Mutex::new(Vec::new()));
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
                    let _: usize = cc.fetch_add(1, Ordering::Relaxed);
                    sf.store(true, Ordering::Relaxed);
                } else {
                    assert_eq!(ans, (rid * 10 + k) * 2);
                    let _: usize = ac.fetch_add(1, Ordering::Relaxed);
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

#[cfg(unix)]
#[test]
fn t4_10_cross_core_mailbox_pipeline_multipool_stop_and_close_race() {
    multipool_detach_and_stop();
    receiver_close_races_senders();
}
