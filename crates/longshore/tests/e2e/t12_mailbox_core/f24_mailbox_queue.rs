// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 24 E2E tests: `longshore_mailbox` queue, wake/drain, park & atomic close.

#[cfg(unix)]
use super::super::harness::posix_backend;
use super::super::harness::TestWorld;
use longshore::io::alloc_guard::sealed;
use longshore::io::engine::{Engine, World};
#[cfg(unix)]
use longshore::io::Completions;
use longshore::io::{FileId, Io, Limits, Op, Sock, Token, Wake};
use longshore::mailbox::{queue, queue_parked, NoWaker, Recv, TryRecv, TrySendError};
use std::collections::BTreeSet;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

struct NoWakerWorld(TestWorld);

impl World for NoWakerWorld {
    fn now_ns(&self) -> u64 {
        self.0.now_ns()
    }
    fn unix_secs(&self) -> u64 {
        self.0.unix_secs()
    }
    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.0.register_file(path)
    }
    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.0.listen(sock, addr)
    }
    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        self.0.listen_unix(sock, path)
    }
    fn waker(&mut self, _woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        None
    }
    fn is_ready(&self, op: &Op) -> bool {
        self.0.is_ready(op)
    }
    fn advance(&mut self) {
        self.0.advance()
    }
    fn first(&mut self, n: usize) -> usize {
        self.0.first(n)
    }
    fn hold(&mut self, op: &Op) -> bool {
        self.0.hold(op)
    }
    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.0.attempt(op, pool, limits)
    }
    fn claimed(&self, op: &Op) -> bool {
        self.0.claimed(op)
    }
    fn release(&mut self, op: &Op) {
        self.0.release(op)
    }
    fn closed(&mut self, sock: Sock) {
        self.0.closed(sock)
    }
    fn wait(&mut self, d: Option<u64>, only_timers: bool, idle: u32) -> io::Result<()> {
        self.0.wait(d, only_timers, idle)
    }
}

#[cfg(unix)]
#[test]
fn t1_f24_1_io_receiver_wakes_blocked_tick_and_drains_message() {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(256)
        .with_ops_max(4);
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    let (tx, mut io_rx) = queue::<u64, _>(&mut io, 8, Token::new(77)).expect("queue");
    io_rx.arm(&mut io).unwrap();

    let h = thread::spawn(move || {
        thread::sleep(Duration::from_millis(15));
        tx.try_send(0xdead_beef_u64).unwrap();
        tx
    });

    io.tick(true, &mut out).unwrap();
    let c = out.pop().expect("AwaitWake completion");
    assert_eq!(c.token(), Token::new(77));
    assert_eq!(c.result(), 0);
    assert!(io_rx.on_completion(&mut io, c).unwrap());
    assert_eq!(io_rx.try_recv(), TryRecv::Message(0xdead_beef_u64));
    assert_eq!(io_rx.try_recv(), TryRecv::Empty);
    drop(h.join().unwrap());
    assert_eq!(io_rx.try_recv(), TryRecv::Closed);
}

#[test]
fn t1_f24_2_single_sender_fifo_order_preserved() {
    let (tx, mut rx) = queue_parked::<u64>(8);
    for v in [10u64, 20, 30, 40] {
        tx.try_send(v).unwrap();
    }
    let mut got = Vec::new();
    while let TryRecv::Message(v) = rx.try_recv() {
        got.push(v);
    }
    assert_eq!(got, vec![10, 20, 30, 40]);
}

#[test]
fn t1_f24_3_four_cloned_senders_deliver_all_messages_once_in_per_sender_order() {
    let (tx, mut rx) = queue_parked::<u64>(32);
    let threads = 4u64;
    let per_thread = 50u64;
    let mut handles = Vec::new();
    for tid in 0..threads {
        let s = tx.clone();
        handles.push(thread::spawn(move || {
            for seq in 0..per_thread {
                let msg = (tid << 16) | seq;
                while let Err(TrySendError::Full(_)) = s.try_send(msg) {
                    thread::yield_now();
                }
            }
        }));
    }

    let mut seen = BTreeSet::new();
    let mut last_seq = [0u64; 4];
    while seen.len() < (threads * per_thread) as usize {
        if let TryRecv::Message(msg) = rx.try_recv() {
            let tid = (msg >> 16) as usize;
            let seq = msg & 0xffff;
            assert_eq!(seq, last_seq[tid], "out-of-order from sender {tid}");
            last_seq[tid] += 1;
            assert!(seen.insert(msg), "duplicate message {msg:#x}");
        } else {
            thread::yield_now();
        }
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(seen.len(), 200);
}

#[test]
fn t1_f24_4_receiver_without_io_wakes_from_park_fallback() {
    let (tx, mut rx) = queue_parked::<u32>(4);
    let h = thread::spawn(move || {
        thread::sleep(Duration::from_millis(15));
        tx.try_send(999).unwrap();
    });
    assert_eq!(rx.recv_parked(), Recv::Message(999));
    h.join().unwrap();
}

#[test]
fn t1_f24_5_queue_send_and_drain_zero_heap_allocations_under_alloc_guard() {
    let (tx, mut rx) = queue_parked::<u64>(8);
    let sum = sealed(|| {
        let mut acc = 0u64;
        for round in 0..100u64 {
            tx.try_send(round).unwrap();
            if let TryRecv::Message(v) = rx.try_recv() {
                acc += v;
            }
        }
        acc
    });
    assert_eq!(sum, (0..100u64).sum::<u64>());
}

#[test]
fn t2_f24_1_full_queue_returns_exact_message_without_perturbing_queued() {
    let (tx, mut rx) = queue_parked::<u64>(3);
    tx.try_send(1).unwrap();
    tx.try_send(2).unwrap();
    tx.try_send(3).unwrap();
    assert_eq!(tx.try_send(999), Err(TrySendError::Full(999)));

    assert_eq!(rx.try_recv(), TryRecv::Message(1));
    assert_eq!(rx.try_recv(), TryRecv::Message(2));
    assert_eq!(rx.try_recv(), TryRecv::Message(3));
    assert_eq!(rx.try_recv(), TryRecv::Empty);
}

#[test]
fn t2_f24_2_stop_succeeds_when_queue_at_full_capacity_and_drains_prior_items() {
    let (tx, mut rx) = queue_parked::<u64>(4);
    for i in 1..=4u64 {
        tx.try_send(i).unwrap();
    }
    assert_eq!(tx.try_send(5), Err(TrySendError::Full(5)));

    // Uses the +1 reserved slot so stop succeeds even at capacity.
    tx.stop();
    assert_eq!(tx.try_send(6), Err(TrySendError::Closed(6)));

    assert_eq!(rx.try_recv(), TryRecv::Message(1));
    assert_eq!(rx.try_recv(), TryRecv::Message(2));
    assert_eq!(rx.try_recv(), TryRecv::Message(3));
    assert_eq!(rx.try_recv(), TryRecv::Message(4));
    assert_eq!(rx.try_recv(), TryRecv::Stop);
}

#[test]
fn t2_f24_3_concurrent_close_vs_send_never_loses_or_duplicates_messages() {
    for round in 0..200u64 {
        let (tx, mut rx) = queue_parked::<u64>(8);
        let s1 = tx.clone();
        let s2 = tx.clone();
        let sent_a = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sent_b = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sa = Arc::clone(&sent_a);
        let sb = Arc::clone(&sent_b);

        let h1 = thread::spawn(move || {
            for k in 0..8u64 {
                let v = (round << 8) | k;
                if s1.try_send(v).is_ok() {
                    sa.lock().unwrap().push(v);
                }
            }
        });
        let h2 = thread::spawn(move || {
            for k in 8..16u64 {
                let v = (round << 8) | k;
                if s2.try_send(v).is_ok() {
                    sb.lock().unwrap().push(v);
                }
            }
        });

        let mut drained = Vec::new();
        while let TryRecv::Message(v) = rx.try_recv() {
            drained.push(v);
        }
        rx.close(|ev| {
            if let Recv::Message(v) = ev {
                drained.push(v);
            }
        });

        h1.join().unwrap();
        h2.join().unwrap();
        while let TryRecv::Message(v) = rx.try_recv() {
            drained.push(v);
        }

        let mut expected = sent_a.lock().unwrap().clone();
        expected.extend_from_slice(&sent_b.lock().unwrap());
        expected.sort_unstable();
        drained.sort_unstable();
        assert_eq!(drained, expected, "mismatch on round {round}");
    }
}

#[test]
fn t2_f24_4_send_after_receiver_close_immediately_returns_closed() {
    let (tx, mut rx) = queue_parked::<u32>(4);
    rx.close(|_| {});
    assert_eq!(tx.try_send(123), Err(TrySendError::Closed(123)));
}

#[test]
fn t2_f24_5_io_receiver_fails_cleanly_when_backend_has_no_waker() {
    let limits = Limits::BASE
        .with_sockets_max(1)
        .with_buffers(1)
        .with_buffer_size(64)
        .with_ops_max(2);
    let mut engine = Engine::with_world(limits, NoWakerWorld(TestWorld::new()));
    assert!(engine.waker().is_none());
    let err = match queue::<u64, _>(&mut engine, 4, Token::new(1)) {
        Ok(_) => panic!("expected error when Io::waker returns None"),
        Err(e) => e,
    };
    assert!(matches!(err, NoWaker));
}
