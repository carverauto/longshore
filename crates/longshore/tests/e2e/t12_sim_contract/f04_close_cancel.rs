// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Feature 4: Close & Cancel `Io` contract (`#41`).

#[cfg(unix)]
use crate::e2e::harness::posix_backend;
use crate::e2e::harness::{drive_until, submit_and_wait_one};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token, ECANCELED};
use longshore::sim::{Faults, SimIo};
use std::net::SocketAddr;
use std::time::Instant;

fn contract_limits() -> Limits {
    Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ..Limits::BASE
    }
}

fn for_each_backend(mut run: impl FnMut(&mut dyn FnMut(Limits) -> Box<dyn Io>, SocketAddr)) {
    let sim_addr: SocketAddr = "192.0.2.20:443".parse().unwrap();
    run(
        &mut |l| Box::new(SimIo::new(l, 7, Faults::default())),
        sim_addr,
    );
    #[cfg(unix)]
    run(
        &mut |l| Box::new(posix_backend(l).expect("posix")),
        "127.0.0.1:0".parse().unwrap(),
    );
    #[cfg(target_os = "linux")]
    run(
        &mut |l| Box::new(UringIo::new(l).expect("uring")),
        "127.0.0.1:0".parse().unwrap(),
    );
}

fn setup_pair<IO: Io + ?Sized>(io: &mut IO, out: &mut Completions, addr: SocketAddr) -> SocketAddr {
    let bound = io.listen(Sock(0), addr).expect("listen");
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(2),
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, out, 1_000, |v| v.len() == 2).expect("pair");
    assert!(cs.iter().all(|c| c.result == 0), "setup failed: {cs:?}");
    bound
}

fn close_all<IO: Io + ?Sized>(io: &mut IO, out: &mut Completions, socks: &[Sock]) {
    for (i, &sock) in socks.iter().enumerate() {
        io.submit(Token(900 + i as u64), Op::Close { sock })
            .unwrap();
    }
    // Teardown: the checks are done, and a slow close does not change them.
    drive_until(io, out, 1_000, |v| v.len() == socks.len()).ok();
}

#[test]
fn t1_f04_1_cancel_waiting_recv_across_backends() {
    for_each_backend(|make, addr| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        setup_pair(&mut *io, &mut out, addr);
        io.submit(
            Token(10),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 512,
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();
        assert!(out.is_empty());

        io.submit(Token(11), Op::Cancel { target: Token(10) })
            .unwrap();
        let mut cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        cs.sort_by_key(|c| c.token.0);
        assert_eq!(
            cs.iter().map(|c| (c.token.0, c.result)).collect::<Vec<_>>(),
            vec![(10, -ECANCELED), (11, 0)]
        );
        close_all(&mut *io, &mut out, &[Sock(2), Sock(1), Sock(0)]);
    });
}

#[test]
fn t1_f04_2_close_connected_sock_cancels_inflight_recv() {
    for_each_backend(|make, addr| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        setup_pair(&mut *io, &mut out, addr);
        io.submit(
            Token(10),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 512,
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();

        io.submit(Token(11), Op::Close { sock: Sock(1) }).unwrap();
        let mut cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        cs.sort_by_key(|c| c.token.0);
        assert_eq!(cs[0].token, Token(10));
        assert!(
            cs[0].result < 0,
            "inflight recv on closed sock must fail: {cs:?}"
        );
        assert_eq!((cs[1].token, cs[1].result), (Token(11), 0));
        close_all(&mut *io, &mut out, &[Sock(2), Sock(0)]);
    });
}

#[test]
fn t1_f04_3_close_listener_cancels_inflight_accept_and_frees_slot() {
    for_each_backend(|make, addr| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        let bound = io.listen(Sock(0), addr).unwrap();
        io.submit(
            Token(20),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(1),
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();
        io.submit(Token(21), Op::Close { sock: Sock(0) }).unwrap();
        let mut cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        cs.sort_by_key(|c| c.token.0);
        assert_eq!(cs[0].token, Token(20));
        assert!(
            cs[0].result < 0,
            "accept on closed listener must fail: {cs:?}"
        );
        assert_eq!((cs[1].token, cs[1].result), (Token(21), 0));

        // Slot 0 is reusable for another listener. It listens on the address
        // the test started from, not on `bound`: with tests running in
        // parallel, another test can be handed the port just freed.
        let rebound = io.listen(Sock(0), addr).expect("relisten on freed slot");
        assert_eq!(rebound.ip(), bound.ip());
        close_all(&mut *io, &mut out, &[Sock(0)]);
    });
}

#[test]
fn t1_f04_4_cancel_long_timeout_completes_immediately() {
    for_each_backend(|make, _| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        let start = Instant::now();
        io.submit(
            Token(30),
            Op::Timeout {
                nanos: 10_000_000_000,
            },
        )
        .unwrap();
        io.submit(Token(31), Op::Cancel { target: Token(30) })
            .unwrap();
        let mut cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        assert!(start.elapsed().as_secs_f32() < 2.0);
        cs.sort_by_key(|c| c.token.0);
        assert_eq!(
            cs.iter().map(|c| (c.token.0, c.result)).collect::<Vec<_>>(),
            vec![(30, -ECANCELED), (31, 0)]
        );
    });
}

#[test]
fn t1_f04_5_cancel_waiting_connect_frees_slot() {
    let addr: SocketAddr = "192.0.2.30:443".parse().unwrap();
    let mut io = SimIo::new(contract_limits(), 5, Faults::default());
    let mut out = Completions::new(8);
    io.listen(Sock(0), addr).unwrap();
    io.submit(
        Token(1),
        Op::Connect {
            sock: Sock(2),
            addr,
        },
    )
    .unwrap();
    for _ in 0..5 {
        io.tick(false, &mut out).unwrap();
    }
    assert!(out.is_empty());

    io.submit(Token(2), Op::Cancel { target: Token(1) })
        .unwrap();
    let mut cs = drive_until(&mut io, &mut out, 100, |v| v.len() == 2).unwrap();
    cs.sort_by_key(|c| c.token.0);
    assert_eq!(
        cs.iter().map(|c| (c.token.0, c.result)).collect::<Vec<_>>(),
        vec![(1, -ECANCELED), (2, 0)]
    );

    // Slot Sock(2) is immediately reusable for a new Connect.
    io.submit(
        Token(3),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    io.submit(
        Token(4),
        Op::Connect {
            sock: Sock(2),
            addr,
        },
    )
    .unwrap();
    let mut cs = drive_until(&mut io, &mut out, 100, |v| v.len() == 2).unwrap();
    cs.sort_by_key(|c| c.token.0);
    assert_eq!(
        cs.iter().map(|c| (c.token.0, c.result)).collect::<Vec<_>>(),
        vec![(3, 0), (4, 0)]
    );
}

#[test]
fn t2_f04_1_cancel_missing_token_returns_negative() {
    for_each_backend(|make, _| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        let c = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(40),
            Op::Cancel {
                target: Token(9999),
            },
            1_000,
        )
        .unwrap();
        assert!(c.result < 0);
        io.tick(false, &mut out).unwrap();
        assert!(out.is_empty());
    });
}

#[test]
fn t2_f04_2_cancel_snapshots_at_submit_spares_later_same_token() {
    for_each_backend(|make, _| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        io.submit(Token(50), Op::Cancel { target: Token(77) })
            .unwrap();
        io.submit(Token(77), Op::Timeout { nanos: 1_000_000 })
            .unwrap();
        let mut cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        cs.sort_by_key(|c| c.token.0);
        assert_eq!(cs[0].token, Token(50));
        assert!(cs[0].result < 0, "snapshot cancel must miss later token");
        assert_eq!((cs[1].token, cs[1].result), (Token(77), 0));
    });
}

#[test]
fn t2_f04_3_close_empty_slot_completes_once() {
    for_each_backend(|make, _| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        let c = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(60),
            Op::Close { sock: Sock(0) },
            100,
        )
        .unwrap();
        assert_eq!(c.token, Token(60));
        io.tick(false, &mut out).unwrap();
        assert!(out.is_empty());
    });
}

#[test]
fn t2_f04_4_cancel_already_completed_op_returns_negative() {
    for_each_backend(|make, _| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        let done = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(70),
            Op::Timeout { nanos: 0 },
            1_000,
        )
        .unwrap();
        assert_eq!(done.result, 0);
        let cancel = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(71),
            Op::Cancel { target: Token(70) },
            1_000,
        )
        .unwrap();
        assert!(cancel.result < 0);
    });
}

#[test]
fn t2_f04_5_double_cancel_same_target_delivers_one_target_completion() {
    for_each_backend(|make, addr| {
        let mut io = make(contract_limits());
        let mut out = Completions::new(8);
        setup_pair(&mut *io, &mut out, addr);
        io.submit(
            Token(80),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 256,
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();

        io.submit(Token(81), Op::Cancel { target: Token(80) })
            .unwrap();
        io.submit(Token(82), Op::Cancel { target: Token(80) })
            .unwrap();
        let mut cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 3).unwrap();
        io.tick(false, &mut out).unwrap();
        assert!(out.is_empty(), "must not emit duplicate completion");
        cs.sort_by_key(|c| c.token.0);
        assert_eq!(cs[0].token, Token(80));
        assert_eq!(cs[0].result, -ECANCELED);
        assert_eq!(cs[1].token, Token(81));
        assert_eq!(cs[2].token, Token(82));
        close_all(&mut *io, &mut out, &[Sock(2), Sock(1), Sock(0)]);
    });
}
