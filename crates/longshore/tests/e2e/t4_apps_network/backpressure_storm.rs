// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-13`: Backpressure & resource-limit storm (`ops_max`, `accept_range`,
//! `ring_buffers`, `Completions`, `Mailbox`, `Pool` + dynamic waiter growth).

use super::super::harness::{drive_until, posix_backend, submit_and_wait_one, write_pool_bytes};
use longshore::io::{
    BufId, Busy, Completion, Completions, Io, Limits, Op, Sock, Token, EMFILE, ENFILE, ENOBUFS,
};
use longshore::mailbox::{mailbox_parked, Config, Recv, TryRecv, TrySendError};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::thread;

fn storm_limits() -> Limits {
    Limits {
        sockets_max: 8,
        buffers: 2,
        buffer_size: 512,
        small_buffers: 2,
        small_buffer_size: 128,
        accept_range_start: 6,
        accept_range_len: 2,
        ring_buffers: 2,
        ring_buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    }
}

fn run_backpressure_storm<IO: Io>(io: &mut IO, addr: SocketAddr) {
    // 1. Saturate ops_max (4) and verify Busy on submit + submit_chain.
    let mut small_out = Completions::new(2);
    for t in 1..=4u64 {
        io.submit(Token(t), Op::Timeout { nanos: 100_000 }).unwrap();
    }
    assert_eq!(
        io.submit(Token(5), Op::Timeout { nanos: 0 }),
        Err(Busy),
        "5th submit must fail with Busy"
    );
    assert_eq!(
        io.submit_chain(&[(
            Token(6),
            Op::FileClose {
                slot: longshore::io::FileSlot(0)
            }
        )]),
        Err(Busy),
        "submit_chain when ops_max full must fail with Busy"
    );

    // 2. Completions::new(2) spillover: 4 ready timers drain cleanly across 2 turns.
    let mut total_drained = 0usize;
    for _ in 0..20 {
        io.tick(true, &mut small_out).unwrap();
        while small_out.pop().is_some() {
            total_drained += 1;
        }
        if total_drained == 4 {
            break;
        }
    }
    assert_eq!(total_drained, 4);

    // 3. Saturate accept_range (len=2) with 3rd connect -> -ENFILE/-EMFILE on armed Accept.
    let bound = io.listen(Sock(0), addr).unwrap();
    io.submit(Token(10), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for i in 0..2u32 {
        io.submit(
            Token(20 + u64::from(i)),
            Op::Connect {
                sock: Sock(1 + i),
                addr: bound,
            },
        )
        .unwrap();
    }
    let mut accepted = Vec::new();
    let mut conn_ok = 0usize;
    while accepted.len() < 2 || conn_ok < 2 {
        io.tick(true, &mut small_out).unwrap();
        while let Some(c) = small_out.pop() {
            if c.token.0 == 10 && c.result >= 0 {
                accepted.push(Sock(c.result as u32));
            } else if c.token.0 >= 20 && c.token.0 < 30 && c.result == 0 {
                conn_ok += 1;
            }
        }
    }

    // 3rd connection finds accept_range full!
    io.submit(
        Token(22),
        Op::Connect {
            sock: Sock(3),
            addr: bound,
        },
    )
    .unwrap();
    let mut accept_ended = false;
    let mut third_res: Option<i32> = None;
    while !accept_ended || third_res.is_none() {
        io.tick(true, &mut small_out).unwrap();
        while let Some(c) = small_out.pop() {
            if c.token.0 == 10 {
                assert!(!c.more);
                assert!(c.result == -ENFILE || c.result == -EMFILE);
                accept_ended = true;
            } else if c.token.0 == 22 {
                third_res = Some(c.result);
            }
        }
    }
    if third_res == Some(0) {
        let peer_end = submit_and_wait_one(
            io,
            &mut small_out,
            Token(23),
            Op::Recv {
                sock: Sock(3),
                buf: BufId(1),
                off: 0,
                len: 16,
            },
            200,
        )
        .unwrap();
        assert!(
            peer_end.result <= 0,
            "rejected 3rd connection must observe EOF or reset, got {}",
            peer_end.result
        );
        submit_and_wait_one(
            io,
            &mut small_out,
            Token(24),
            Op::Close { sock: Sock(3) },
            200,
        )
        .expect("close the rejected connection");
    } else {
        assert!(third_res.unwrap() < 0);
    }

    // 4. Saturate ring_buffers (2) -> 3rd message triggers -ENOBUFS, give_back recovers.
    io.submit(Token(30), Op::ArmedRecv { sock: accepted[0] })
        .unwrap();
    io.submit(Token(31), Op::ArmedRecv { sock: accepted[1] })
        .unwrap();

    let mut held_bufs = Vec::new();
    let mut saw_enobufs = false;
    for k in 0..4u64 {
        write_pool_bytes(io, BufId(0), 0, &[0x90 + k as u8; 64]);
        let send_tok = Token(40 + k);
        io.submit(
            send_tok,
            Op::Send {
                sock: Sock(1 + (k as u32 % 2)),
                buf: BufId(0),
                off: 0,
                len: 64,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(io, &mut small_out, 200, |v| {
            v.iter().any(|c| c.token == send_tok)
                && v.iter().any(|c| c.buf.is_some() || c.result == -ENOBUFS)
        })
        .unwrap();
        for c in cs {
            if let Some(b) = c.buf {
                held_bufs.push(b);
            }
            if c.result == -ENOBUFS {
                saw_enobufs = true;
            }
        }
        if saw_enobufs {
            break;
        }
    }
    assert!(saw_enobufs, "ring exhaustion must trigger -ENOBUFS");

    // Give buffers back & close one accepted socket -> system recovers cleanly.
    for b in held_bufs {
        io.give_back(b);
    }
    let _: Completion = submit_and_wait_one(
        io,
        &mut small_out,
        Token(80),
        Op::Close { sock: accepted[1] },
        200,
    )
    .unwrap();
    io.submit(Token(81), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(82),
        Op::Connect {
            sock: Sock(4),
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, &mut small_out, 200, |v| {
        v.iter().any(|c| c.token.0 == 81 && c.result >= 0)
            && v.iter().any(|c| c.token.0 == 82 && c.result == 0)
    })
    .unwrap();
    assert!(cs.iter().any(|c| c.token.0 == 81 && c.result >= 0));

    // 5. Mailbox (cap=2) + Pool (slots=1, waiters=1) dynamic waiter growth storm.
    let (tx, mut io_rx) = longshore::mailbox::queue::<u32, _>(io, 2, Token(90)).unwrap();
    io_rx.arm(io).unwrap();
    assert_eq!(tx.try_send(10), Ok(()));
    assert_eq!(tx.try_send(20), Ok(()));
    assert_eq!(tx.try_send(30), Err(TrySendError::Full(30)));
    assert_eq!(io_rx.try_recv(), TryRecv::Message(10));
    assert_eq!(io_rx.try_recv(), TryRecv::Message(20));
    io_rx.close(|_| {});

    let (tx_p, mut rx_p) = mailbox_parked::<(u32, u32), u32>(Config {
        capacity: 8,
        slots: 1,
        waiters_prealloc: 1,
        wake_token: Token(0),
    });
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut f0 = pin!(tx_p.call(|slot| (slot, 99u32), 0));
    assert!(matches!(f0.as_mut().poll(&mut cx), Poll::Pending));
    let TryRecv::Message((s0_slot, _)) = rx_p.try_recv() else {
        panic!("expected initial request");
    };

    let mut threads = Vec::new();
    for id in 0..3u32 {
        let t = tx_p.clone();
        threads.push(thread::spawn(move || {
            t.call(|slot| (slot, id + 100), 0).wait()
        }));
    }
    let responder = thread::spawn(move || {
        for _ in 0..3 {
            let Recv::Message((slot, val)) = rx_p.recv_parked() else {
                panic!("unexpected non-message");
            };
            assert!(rx_p.answer(slot, val).is_none());
        }
    });
    // Release initial slot after brief yield so all 3 waiters (exceeding prealloc=1) take turns.
    thread::yield_now();
    assert!(tx_p.cells().answer(tx_p.pool(), s0_slot, 99).is_none());
    assert_eq!(longshore::mailbox::block_on(f0), 99);
    responder.join().unwrap();
    let mut vals: Vec<u32> = threads.into_iter().map(|h| h.join().unwrap()).collect();
    vals.sort_unstable();
    assert_eq!(vals, vec![100, 101, 102]);
}

#[cfg(feature = "sim")]
#[test]
fn t4_13_backpressure_and_resource_limit_storm_sim() {
    let mut sim = SimIo::new(storm_limits(), 413, Faults::default());
    run_backpressure_storm(&mut sim, "127.0.0.1:9413".parse().unwrap());
}

#[cfg(unix)]
#[test]
fn t4_13_backpressure_and_resource_limit_storm_posix() {
    let mut posix = posix_backend(storm_limits()).unwrap();
    run_backpressure_storm(&mut posix, "127.0.0.1:0".parse().unwrap());
}
