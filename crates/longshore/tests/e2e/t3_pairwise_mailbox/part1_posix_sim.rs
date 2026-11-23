// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-21`–`T3-22`: `Posix` waker `AwaitWake` over
//! 100 messages, and 2-core `Engine<Virtual>` `Pool`/`Future` replay parity.

use super::PingMsg;
use longshore::io::{Completions, Io, Limits, Token};
use longshore::mailbox::{mailbox, queue, Config, TryRecv, TrySendError};

#[cfg(unix)]
#[test]
fn t3_21_posix_waker_mailbox_await_wake_100_msgs() {
    use super::super::harness::posix_backend;

    let limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 64,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let (tx, mut io_rx) = queue::<PingMsg, _>(&mut io, 32, Token(1)).expect("queue");
    io_rx.arm(&mut io).expect("arm initial AwaitWake");

    let producer = std::thread::spawn(move || {
        for seq in 0..100u64 {
            let mut msg = PingMsg { seq, slot: 0 };
            loop {
                match tx.try_send(msg) {
                    Ok(()) => break,
                    Err(TrySendError::Full(returned)) => {
                        msg = returned;
                        std::thread::yield_now();
                    }
                    Err(TrySendError::Closed(_)) => panic!("closed early"),
                }
            }
        }
    });

    let mut out = Completions::new(8);
    let mut received = Vec::with_capacity(100);
    for _ in 0..10_000 {
        if received.len() == 100 {
            break;
        }
        io.tick(true, &mut out).expect("posix tick");
        while let Some(c) = out.pop() {
            if io_rx.on_completion(&mut io, c).unwrap() {
                loop {
                    match io_rx.try_recv() {
                        TryRecv::Message(msg) => received.push(msg),
                        TryRecv::Stop => panic!("unexpected stop"),
                        TryRecv::Empty | TryRecv::Closed => break,
                    }
                }
            }
        }
    }
    io_rx.close(|_| {});
    producer.join().unwrap();
    assert_eq!(received.len(), 100);
    for (idx, msg) in received.iter().enumerate() {
        assert_eq!(msg.seq, idx as u64);
    }
}

#[cfg(feature = "sim")]
#[test]
fn t3_22_two_virtual_engines_mailbox_pool_future_trace_parity() {
    use longshore::mailbox::block_on;
    use longshore::sim::{Faults, SimIo};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    fn run_once(seed_a: u64, seed_b: u64) -> (u64, u64, Vec<u64>) {
        let limits = Limits {
            sockets_max: 2,
            buffers: 1,
            buffer_size: 64,
            ops_max: 4,
            ..Limits::BASE
        };
        let mut core_a = SimIo::new(limits, seed_a, Faults::default());
        let mut core_b = SimIo::new(limits, seed_b, Faults::default());
        let (tx_b, mut io_rx_b) = mailbox::<PingMsg, u64, _>(
            &mut core_b,
            Config {
                capacity: 8,
                slots: 4,
                waiters_prealloc: 8,
                wake_token: Token(1),
            },
        )
        .unwrap();
        io_rx_b.arm(&mut core_b).unwrap();
        let mut out_a = Completions::new(4);
        let mut out_b = Completions::new(4);
        let mut answers = Vec::new();
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        for seq in 0..20u64 {
            let mut fut = pin!(tx_b.call(|slot| PingMsg { seq, slot }, 0));
            assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

            core_a.tick(false, &mut out_a).unwrap();
            core_b.tick(false, &mut out_b).unwrap();
            while let Some(c) = out_b.pop() {
                if io_rx_b.on_completion(&mut core_b, c).unwrap() {
                    while let TryRecv::Message(msg) = io_rx_b.try_recv() {
                        assert!(io_rx_b.answer(msg.slot, msg.seq * 3 + 1).is_none());
                    }
                }
            }
            core_a.tick(false, &mut out_a).unwrap();
            let ans = block_on(fut);
            answers.push(ans);
        }
        (core_a.trace_hash(), core_b.trace_hash(), answers)
    }

    let (ha1, hb1, ans1) = run_once(11, 22);
    let (ha2, hb2, ans2) = run_once(11, 22);
    assert_eq!(ha1, ha2);
    assert_eq!(hb1, hb2);
    assert_eq!(ans1, ans2);
    assert_eq!(ans1.len(), 20);
}
