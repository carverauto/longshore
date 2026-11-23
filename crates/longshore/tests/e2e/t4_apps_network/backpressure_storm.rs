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
    Limits::BASE
        .with_sockets_max(8)
        .with_buffers(2)
        .with_buffer_size(512)
        .with_small_buffers(2)
        .with_small_buffer_size(128)
        .with_accept_range_start(6)
        .with_accept_range_len(2)
        .with_ring_buffers(2)
        .with_ring_buffer_size(256)
        .with_ops_max(4)
}

/// Saturates `ops_max` (4) and checks `Busy` on both `submit` and `submit_chain`.
fn saturate_ops_max<IO: Io>(io: &mut IO) {
    for t in 1..=4u64 {
        io.submit(Token::new(t), Op::Timeout { nanos: 100_000 })
            .unwrap();
    }
    assert_eq!(
        io.submit(Token::new(5), Op::Timeout { nanos: 0 }),
        Err(Busy),
        "5th submit must fail with Busy"
    );
    assert_eq!(
        io.submit_chain(&[(
            Token::new(6),
            Op::FileClose {
                slot: longshore::io::FileSlot::new(0)
            }
        )]),
        Err(Busy),
        "submit_chain when ops_max full must fail with Busy"
    );
}

/// `Completions::new(2)` spillover: 4 ready timers drain cleanly across 2 turns.
fn drain_spillover<IO: Io>(io: &mut IO, small_out: &mut Completions) {
    let mut total_drained = 0usize;
    for _ in 0..20 {
        io.tick(true, small_out).unwrap();
        while small_out.pop().is_some() {
            total_drained += 1;
        }
        if total_drained == 4 {
            break;
        }
    }
    assert_eq!(total_drained, 4);
}

/// Fills the accept range (len=2) with two armed-accepted connections;
/// returns the bound address and the accepted sockets.
fn fill_accept_range<IO: Io>(
    io: &mut IO,
    small_out: &mut Completions,
    addr: SocketAddr,
) -> (SocketAddr, Vec<Sock>) {
    let bound = io.listen(Sock::new(0), addr).unwrap();
    io.submit(
        Token::new(10),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    for i in 0..2u32 {
        io.submit(
            Token::new(20 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr: bound,
            },
        )
        .unwrap();
    }
    let mut accepted = Vec::new();
    let mut conn_ok = 0usize;
    while accepted.len() < 2 || conn_ok < 2 {
        io.tick(true, small_out).unwrap();
        while let Some(c) = small_out.pop() {
            if c.token().get() == 10 && c.result() >= 0 {
                accepted.push(Sock::new(u32::try_from(c.result()).unwrap()));
            } else if c.token().get() >= 20 && c.token().get() < 30 && c.result() == 0 {
                conn_ok += 1;
            }
        }
    }
    (bound, accepted)
}

/// A 3rd connection finds the accept range full: the armed accept ends with
/// `-ENFILE`/`-EMFILE`, and the rejected client sees EOF/reset or fails.
fn third_connect_is_rejected<IO: Io>(io: &mut IO, small_out: &mut Completions, bound: SocketAddr) {
    io.submit(
        Token::new(22),
        Op::Connect {
            sock: Sock::new(3),
            addr: bound,
        },
    )
    .unwrap();
    let mut accept_ended = false;
    let mut third_res: Option<i32> = None;
    while !accept_ended || third_res.is_none() {
        io.tick(true, small_out).unwrap();
        while let Some(c) = small_out.pop() {
            if c.token().get() == 10 {
                assert!(!c.more());
                assert!(c.result() == -ENFILE || c.result() == -EMFILE);
                accept_ended = true;
            } else if c.token().get() == 22 {
                third_res = Some(c.result());
            }
        }
    }
    if third_res == Some(0) {
        let peer_end = submit_and_wait_one(
            io,
            small_out,
            Token::new(23),
            Op::Recv {
                sock: Sock::new(3),
                buf: BufId::new(1),
                off: 0,
                len: 16,
            },
            200,
        )
        .unwrap();
        assert!(
            peer_end.result() <= 0,
            "rejected 3rd connection must observe EOF or reset, got {}",
            peer_end.result()
        );
        let _: Completion = submit_and_wait_one(
            io,
            small_out,
            Token::new(24),
            Op::Close { sock: Sock::new(3) },
            200,
        )
        .expect("close the rejected connection");
    } else {
        assert!(third_res.unwrap() < 0);
    }
}

/// Saturates the ring buffers (2) until a message triggers `-ENOBUFS`;
/// returns the ring buffers still held.
fn exhaust_ring_buffers<IO: Io>(
    io: &mut IO,
    small_out: &mut Completions,
    accepted: &[Sock],
) -> Vec<BufId> {
    io.submit(Token::new(30), Op::ArmedRecv { sock: accepted[0] })
        .unwrap();
    io.submit(Token::new(31), Op::ArmedRecv { sock: accepted[1] })
        .unwrap();

    let mut held_bufs = Vec::new();
    let mut saw_enobufs = false;
    for k in 0..4u64 {
        write_pool_bytes(io, BufId::new(0), 0, &[0x90 + k as u8; 64]);
        let send_tok = Token::new(40 + k);
        io.submit(
            send_tok,
            Op::Send {
                sock: Sock::new(1 + (k as u32 % 2)),
                buf: BufId::new(0),
                off: 0,
                len: 64,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(io, small_out, 200, |v| {
            v.iter().any(|c| c.token() == send_tok)
                && v.iter()
                    .any(|c| c.buf().is_some() || c.result() == -ENOBUFS)
        })
        .unwrap();
        for c in cs {
            if let Some(b) = c.buf() {
                held_bufs.push(b);
            }
            if c.result() == -ENOBUFS {
                saw_enobufs = true;
            }
        }
        if saw_enobufs {
            break;
        }
    }
    assert!(saw_enobufs, "ring exhaustion must trigger -ENOBUFS");
    held_bufs
}

/// Gives the buffers back and closes one accepted socket; a fresh armed
/// accept and connect then succeed.
fn recover_after_storm<IO: Io>(
    io: &mut IO,
    small_out: &mut Completions,
    held_bufs: Vec<BufId>,
    accepted: &[Sock],
    bound: SocketAddr,
) {
    for b in held_bufs {
        io.give_back(b);
    }
    let _: Completion = submit_and_wait_one(
        io,
        small_out,
        Token::new(80),
        Op::Close { sock: accepted[1] },
        200,
    )
    .unwrap();
    io.submit(
        Token::new(81),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(
        Token::new(82),
        Op::Connect {
            sock: Sock::new(4),
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, small_out, 200, |v| {
        v.iter().any(|c| c.token().get() == 81 && c.result() >= 0)
            && v.iter().any(|c| c.token().get() == 82 && c.result() == 0)
    })
    .unwrap();
    assert!(cs.iter().any(|c| c.token().get() == 81 && c.result() >= 0));
}

/// A capacity-2 queue refuses the 3rd message with `Full`.
fn full_queue_refuses<IO: Io>(io: &mut IO) {
    let (tx, mut io_rx) = longshore::mailbox::queue::<u32, _>(io, 2, Token::new(90)).unwrap();
    io_rx.arm(io).unwrap();
    assert_eq!(tx.try_send(10), Ok(()));
    assert_eq!(tx.try_send(20), Ok(()));
    assert_eq!(tx.try_send(30), Err(TrySendError::Full(30)));
    assert_eq!(io_rx.try_recv(), TryRecv::Message(10));
    assert_eq!(io_rx.try_recv(), TryRecv::Message(20));
    io_rx.close(|_| {});
}

/// A one-slot pool with one preallocated waiter serves three more waiting
/// callers, growing its waiter queue.
fn pool_waiters_grow() {
    let (tx_p, mut rx_p) = mailbox_parked::<(u32, u32), u32>(Config::new(8, 1, 1, Token::new(0)));
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

fn run_backpressure_storm<IO: Io>(io: &mut IO, addr: SocketAddr) {
    let mut small_out = Completions::new(2);
    saturate_ops_max(io);
    drain_spillover(io, &mut small_out);
    let (bound, accepted) = fill_accept_range(io, &mut small_out, addr);
    third_connect_is_rejected(io, &mut small_out, bound);
    let held_bufs = exhaust_ring_buffers(io, &mut small_out, &accepted);
    recover_after_storm(io, &mut small_out, held_bufs, &accepted, bound);
    full_queue_refuses(io);
    pool_waiters_grow();
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
