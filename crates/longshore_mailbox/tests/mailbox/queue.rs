// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Functional tests for the bounded lock-free MPSC queue, `Io` `AwaitWake`
//! integration (with zero timers), `park` fallback, `stop`, and `close`.

#![cfg(not(longshore_model))]

use longshore_common::io::posix::PosixIo;
use longshore_common::io::{BufId, Busy, Completions, FileId, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_mailbox::{
    mailbox, queue, queue_parked, queue_with_wake, NoWaker, Recv, TryRecv, TrySendError,
};
use std::io;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

struct StubNoWakerIo;

impl Io for StubNoWakerIo {
    fn now_ns(&self) -> u64 {
        0
    }
    fn unix_secs(&self) -> u64 {
        0
    }
    fn buf(&mut self, _: BufId) -> &mut [u8] {
        &mut []
    }
    fn buf_pair(&mut self, _: BufId, _: BufId) -> (&mut [u8], &mut [u8]) {
        (&mut [], &mut [])
    }
    fn register_file(&mut self, _: &str) -> io::Result<FileId> {
        Err(io::Error::other("unused"))
    }
    fn listen(&mut self, _: Sock, _: SocketAddr) -> io::Result<SocketAddr> {
        Err(io::Error::other("unused"))
    }
    fn listen_unix(&mut self, _: Sock, _: &str) -> io::Result<()> {
        Err(io::Error::other("unused"))
    }
    fn give_back(&mut self, _: BufId) {}
    fn submit(&mut self, _: Token, _: Op) -> Result<(), Busy> {
        Ok(())
    }
    fn submit_chain(&mut self, _: &[(Token, Op)]) -> Result<(), Busy> {
        Ok(())
    }
    fn tick(&mut self, _: bool, _: &mut Completions) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn build_fails_when_io_has_no_waker() {
    let mut io = StubNoWakerIo;
    assert!(matches!(
        queue::<u32, _>(&mut io, 4, Token::new(1)),
        Err(NoWaker)
    ));
    let cfg = longshore_mailbox::Config {
        capacity: 4,
        slots: 2,
        waiters_prealloc: 4,
        wake_token: Token::new(1),
    };
    assert!(matches!(mailbox::<u32, u32, _>(&mut io, cfg), Err(NoWaker)));
}

fn test_limits() -> Limits {
    Limits::BASE.with_ops_max(4)
}

#[test]
fn sim_io_wakes_and_drains_without_any_timer() {
    let mut io = SimIo::new(test_limits(), 42, Faults::default());
    let (tx, mut rx) = queue::<u32, _>(&mut io, 4, Token::new(99)).unwrap();
    rx.arm(&mut io).unwrap();

    // Signal arrives while no timer is submitted.
    tx.try_send(10).unwrap();
    tx.try_send(20).unwrap();
    tx.try_send(30).unwrap();

    let mut out = Completions::new(16);
    io.tick(true, &mut out).unwrap();
    let c = out.pop().expect("AwaitWake completion");
    assert_eq!(c.token(), Token::new(99));
    assert_eq!(c.result(), 0);
    assert!(rx.on_completion(&mut io, c).unwrap());

    let mut got = Vec::new();
    let closed = rx.drain(|item| match item {
        Recv::Message(v) => got.push(v),
        other @ (Recv::Stop | Recv::Closed) => panic!("unexpected {other:?}"),
    });
    assert!(!closed);
    assert_eq!(got, vec![10, 20, 30]);
}

#[test]
fn posix_io_wakes_from_another_thread_without_any_timer() {
    let mut io = PosixIo::new(test_limits()).unwrap();
    let (tx, mut rx) = queue::<u64, _>(&mut io, 4, Token::new(7)).unwrap();
    rx.arm(&mut io).unwrap();

    let (ready_tx, ready_rx) = mpsc::channel();
    let t = std::thread::spawn(move || {
        ready_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        tx.wake();
        tx.try_send(111).unwrap();
        tx.try_send(222).unwrap();
        tx.stop();
    });

    ready_tx.send(()).unwrap();
    let mut out = Completions::new(16);
    let mut got = Vec::new();
    let mut stopped = false;
    while !stopped {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert!(rx.on_completion(&mut io, c).unwrap());
        }
        let _: bool = rx.drain(|item| match item {
            Recv::Message(v) => got.push(v),
            Recv::Stop => stopped = true,
            Recv::Closed => panic!("unexpected Closed before Stop"),
        });
    }
    t.join().unwrap();
    assert_eq!(got, vec![111, 222]);
}

#[test]
fn full_queue_returns_message_and_accepts_in_band_stop() {
    let (tx, mut rx) = queue_parked::<u32>(2);
    assert_eq!(tx.try_send(1), Ok(()));
    assert_eq!(tx.try_send(2), Ok(()));
    assert_eq!(tx.try_send(3), Err(TrySendError::Full(3)));

    // Stop succeeds even when queue is at capacity (+1 reserved slot).
    tx.stop();
    assert_eq!(tx.try_send(4), Err(TrySendError::Closed(4)));

    assert_eq!(rx.try_recv(), TryRecv::Message(1));
    assert_eq!(rx.try_recv(), TryRecv::Message(2));
    assert_eq!(rx.try_recv(), TryRecv::Stop);
    assert_eq!(rx.try_recv(), TryRecv::Closed);
}

#[test]
fn recv_parked_and_wait_stop_without_io() {
    {
        let (tx, mut rx) = queue_parked::<u32>(2);
        let tx2 = tx.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            tx2.try_send(5).unwrap();
            std::thread::sleep(Duration::from_millis(15));
            tx2.stop();
        });

        assert_eq!(rx.recv_parked(), Recv::Message(5));
        assert_eq!(rx.recv_parked(), Recv::Stop);
        h.join().unwrap();
    }

    // Closing before stop() still lets wait_stop() wake when stop() arrives.
    {
        let (tx, mut rx) = queue_parked::<u32>(2);
        rx.close(|_| {});
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            tx.stop();
        });
        rx.wait_stop();
        h.join().unwrap();
    }

    // Dropping all senders wakes wait_stop() too.
    {
        let (tx, mut rx) = queue_parked::<u32>(2);
        rx.close(|_| {});
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            drop(tx);
        });
        rx.wait_stop();
        h.join().unwrap();
    }
}

#[test]
fn close_drains_accepted_messages_and_refuses_later_sends() {
    let (tx, mut rx) = queue_parked::<u32>(3);
    tx.try_send(10).unwrap();
    tx.try_send(20).unwrap();
    let mut drained = Vec::new();
    rx.close(|item| {
        if let Recv::Message(v) = item {
            drained.push(v);
        }
    });
    assert_eq!(drained, vec![10, 20]);
    assert_eq!(tx.try_send(30), Err(TrySendError::Closed(30)));
}

#[test]
fn try_send_racing_stop_never_delivers_after_stop() {
    for _ in 0..15_000 {
        let (tx1, mut rx) = queue_parked::<u32>(4);
        let tx2 = tx1.clone();
        let mut before_stop = Vec::new();
        let mut after_stop = Vec::new();
        let mut stopped = false;

        let accepted = std::thread::scope(|s| {
            let sender = s.spawn(move || {
                let mut sent = Vec::new();
                for seq in 0..32u32 {
                    loop {
                        match tx1.try_send(seq) {
                            Ok(()) => {
                                sent.push(seq);
                                break;
                            }
                            Err(TrySendError::Full(_)) => std::thread::yield_now(),
                            Err(TrySendError::Closed(_)) => return sent,
                        }
                    }
                }
                sent
            });
            let stopper = s.spawn(move || {
                tx2.stop();
            });

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
                    TryRecv::Empty => std::thread::yield_now(),
                    TryRecv::Closed => break,
                }
            }
            stopper.join().unwrap();
            sender.join().unwrap()
        });

        assert!(stopped, "Stop must be observed");
        assert!(
            after_stop.is_empty(),
            "messages delivered after Stop: {after_stop:?}"
        );
        assert_eq!(before_stop, accepted);
    }
}

#[test]
#[should_panic(expected = "queue_parked or mailbox_parked")]
fn recv_parked_panics_on_queue_with_wake() {
    struct NoopWake;
    impl longshore_common::io::Wake for NoopWake {
        fn wake(&self) {}
    }

    let (_tx, mut rx) = queue_with_wake::<u32>(std::sync::Arc::new(NoopWake), 2, Token::new(1));
    let _: Recv<u32> = rx.recv_parked();
}

#[path = "queue/adversarial.rs"]
mod adversarial;
