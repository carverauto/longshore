// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `loom` model tests for the bounded lock-free queue, in-band `stop`,
//! atomic `close` vs `send`, `wait_stop` race, waiter slots, detach ordering,
//! and closing unanswered cells.

#![cfg(longshore_model)]

use longshore_mailbox::{queue_parked, Recv, TryRecv, TrySendError};

#[path = "model/slots.rs"]
mod slots;

fn model(bound: usize, f: impl Fn() + Send + Sync + 'static) {
    let mut b = loom::model::Builder::new();
    b.preemption_bound = Some(bound);
    b.check(f);
}

#[test]
fn queue_two_senders_and_receiver() {
    model(2, || {
        let (tx1, mut rx) = queue_parked::<u32>(2);
        let tx2 = tx1.clone();
        let h1 = loom::thread::spawn(move || {
            tx1.try_send(10).unwrap();
        });
        let h2 = loom::thread::spawn(move || {
            tx2.try_send(20).unwrap();
        });
        let mut got = Vec::new();
        while got.len() < 2 {
            match rx.recv_parked() {
                Recv::Message(v) => got.push(v),
                other @ (Recv::Stop | Recv::Closed) => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(rx.recv_parked(), Recv::Closed);
        got.sort_unstable();
        assert_eq!(got, vec![10, 20]);
        h1.join().unwrap();
        h2.join().unwrap();
    });
}

#[test]
fn queue_full_and_wraparound() {
    model(2, || {
        let (tx, mut rx) = queue_parked::<u32>(1);
        tx.try_send(1).unwrap();
        assert_eq!(tx.try_send(99), Err(TrySendError::Full(99)));
        assert_eq!(rx.recv_parked(), Recv::Message(1));

        let h = loom::thread::spawn(move || {
            tx.try_send(2).unwrap();
            // After cap=1 is drained, wraps around to slot 0/1.
            assert!(matches!(
                tx.try_send(3),
                Ok(()) | Err(TrySendError::Full(3))
            ));
        });
        assert_eq!(rx.recv_parked(), Recv::Message(2));
        match rx.recv_parked() {
            Recv::Message(3) => assert_eq!(rx.recv_parked(), Recv::Closed),
            Recv::Closed => {}
            other @ (Recv::Message(_) | Recv::Stop) => panic!("unexpected {other:?}"),
        }
        h.join().unwrap();
    });
}

#[test]
fn queue_stop_when_full_and_refuses_after() {
    model(2, || {
        let (tx, mut rx) = queue_parked::<u32>(1);
        tx.try_send(7).unwrap();
        assert_eq!(tx.try_send(8), Err(TrySendError::Full(8)));
        let h = loom::thread::spawn(move || {
            tx.stop();
            assert_eq!(tx.try_send(9), Err(TrySendError::Closed(9)));
        });
        assert_eq!(rx.recv_parked(), Recv::Message(7));
        assert_eq!(rx.recv_parked(), Recv::Stop);
        assert_eq!(rx.recv_parked(), Recv::Closed);
        h.join().unwrap();
    });
}

#[test]
fn queue_send_racing_close() {
    model(2, || {
        let (tx1, mut rx) = queue_parked::<u32>(2);
        let tx2 = tx1.clone();
        let h1 = loom::thread::spawn(move || tx1.try_send(1));
        let h2 = loom::thread::spawn(move || tx2.try_send(2));

        let mut taken = Vec::new();
        while let TryRecv::Message(v) = rx.try_recv() {
            taken.push(v);
        }
        rx.close(|item| {
            if let Recv::Message(v) = item {
                taken.push(v);
            }
        });

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        for (val, res) in [(1, r1), (2, r2)] {
            match res {
                Ok(()) => assert!(taken.contains(&val), "accepted msg {val} lost"),
                Err(TrySendError::Closed(v)) => {
                    assert_eq!(v, val);
                    assert!(!taken.contains(&val), "refused msg {val} also taken");
                }
                Err(TrySendError::Full(_)) => panic!("cap 2 cannot be full"),
            }
        }
    });
}

#[test]
fn queue_close_and_wait_stop_racing_stop() {
    model(2, || {
        let (tx, mut rx) = queue_parked::<u32>(1);
        let h = loom::thread::spawn(move || {
            tx.stop();
        });
        rx.close(|_| {});
        rx.wait_stop();
        h.join().unwrap();
    });
}

#[test]
fn queue_try_send_racing_stop_try_recv() {
    model(2, || {
        let (tx1, mut rx) = queue_parked::<u32>(2);
        let tx2 = tx1.clone();
        let h1 = loom::thread::spawn(move || tx1.try_send(42));
        let h2 = loom::thread::spawn(move || tx2.stop());

        let mut before_stop = Vec::new();
        let mut after_stop = Vec::new();
        let mut stopped = false;
        loop {
            match rx.try_recv() {
                TryRecv::Message(v) => {
                    if stopped {
                        after_stop.push(v);
                    } else {
                        before_stop.push(v);
                    }
                }
                TryRecv::Stop => {
                    assert!(!stopped, "duplicate Stop");
                    stopped = true;
                }
                TryRecv::Empty => loom::thread::yield_now(),
                TryRecv::Closed => break,
            }
        }
        let r1 = h1.join().unwrap();
        h2.join().unwrap();
        assert!(stopped, "Stop must be observed");
        assert!(after_stop.is_empty(), "messages after Stop: {after_stop:?}");
        match r1 {
            Ok(()) => assert_eq!(before_stop, vec![42]),
            Err(TrySendError::Closed(42)) => assert!(before_stop.is_empty()),
            other => panic!("unexpected try_send result: {other:?}"),
        }
    });
}

#[test]
fn queue_try_send_racing_stop_close() {
    model(2, || {
        let (tx1, mut rx) = queue_parked::<u32>(1);
        let tx2 = tx1.clone();
        let h1 = loom::thread::spawn(move || tx1.try_send(42));
        let h2 = loom::thread::spawn(move || tx2.stop());

        let mut items = Vec::new();
        rx.close(|item| items.push(item));

        let r1 = h1.join().unwrap();
        h2.join().unwrap();
        if let Some(pos) = items.iter().position(|i| matches!(i, Recv::Stop)) {
            assert_eq!(pos + 1, items.len(), "items after Stop: {items:?}");
        }
        let msgs: Vec<u32> = items
            .iter()
            .filter_map(|i| match *i {
                Recv::Message(v) => Some(v),
                Recv::Stop | Recv::Closed => None,
            })
            .collect();
        match r1 {
            Ok(()) => assert_eq!(msgs, vec![42]),
            Err(TrySendError::Closed(42)) => assert!(msgs.is_empty()),
            other => panic!("unexpected try_send result: {other:?}"),
        }
    });
}

#[test]
fn queue_try_send_racing_stop_try_recv_then_close() {
    model(2, || {
        let (tx1, mut rx) = queue_parked::<u32>(2);
        let tx2 = tx1.clone();
        let h1 = loom::thread::spawn(move || tx1.try_send(42));
        let h2 = loom::thread::spawn(move || tx2.stop());

        let mut items = Vec::new();
        match rx.try_recv() {
            TryRecv::Message(v) => items.push(Recv::Message(v)),
            TryRecv::Stop => items.push(Recv::Stop),
            TryRecv::Empty | TryRecv::Closed => {}
        }
        rx.close(|item| items.push(item));

        let r1 = h1.join().unwrap();
        h2.join().unwrap();
        let stop_count = items.iter().filter(|i| matches!(i, Recv::Stop)).count();
        assert!(stop_count <= 1, "duplicate Stop: {items:?}");
        if let Some(pos) = items.iter().position(|i| matches!(i, Recv::Stop)) {
            assert_eq!(pos + 1, items.len(), "items after Stop: {items:?}");
        }
        let msgs: Vec<u32> = items
            .iter()
            .filter_map(|i| match *i {
                Recv::Message(v) => Some(v),
                Recv::Stop | Recv::Closed => None,
            })
            .collect();
        match r1 {
            Ok(()) => assert_eq!(msgs, vec![42]),
            Err(TrySendError::Closed(42)) => assert!(msgs.is_empty()),
            other => panic!("unexpected try_send result: {other:?}"),
        }
    });
}

#[test]
fn queue_concurrent_stops_emit_at_most_one_stop() {
    model(2, || {
        let (tx1, mut rx) = queue_parked::<u32>(2);
        let tx2 = tx1.clone();
        assert!(tx1.try_send(7).is_ok(), "the queue holds two");
        let h1 = loom::thread::spawn(move || tx1.stop());
        let h2 = loom::thread::spawn(move || tx2.stop());
        let mut stops = 0;
        let mut msgs = Vec::new();
        loop {
            match rx.recv_parked() {
                Recv::Message(m) => msgs.push(m),
                Recv::Stop => stops += 1,
                Recv::Closed => break,
            }
        }
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(stops, 1);
        assert_eq!(msgs, vec![7]);
    });
}

#[test]
fn queue_recv_parked_races_wake_try_send_and_last_sender_drop() {
    model(2, || {
        let (tx, mut rx) = queue_parked::<u32>(2);
        let h = loom::thread::spawn(move || {
            tx.wake();
            assert!(tx.try_send(9).is_ok(), "the queue holds two");
            drop(tx);
        });
        let mut msgs = Vec::new();
        while let Recv::Message(v) = rx.recv_parked() {
            msgs.push(v);
        }
        h.join().unwrap();
        assert_eq!(msgs, vec![9]);
    });
}
