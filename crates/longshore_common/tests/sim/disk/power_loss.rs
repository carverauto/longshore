// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Whole-machine `power_loss` tests: dropping in-flight ops, freeing sockets
//! and file slots, clearing timers/waker, and rebuilding both ends of a stream.

use super::{drive, make_sim, open_path, rw_create, rw_open, sync_dir_path};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op, Sock, Token, EPIPE};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9000);

#[test]
fn power_loss_drops_in_flight_ops_frees_slots_and_rebuilds_callers() {
    let mut sim = make_sim(19, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    let waker = sim.waker().unwrap();

    sim.listen(Sock(0), ADDR).unwrap();
    sim.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    sim.submit(
        Token(2),
        Op::Connect {
            sock: Sock(2),
            addr: ADDR,
        },
    )
    .unwrap();
    let mut connected = 0;
    while connected < 2 {
        sim.tick(true, &mut out).unwrap();
        while out.pop().is_some() {
            connected += 1;
        }
    }

    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            3,
            FileSlot(0),
            "/durable.dat",
            rw_create()
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sim.buf(BufId(1))[..8].copy_from_slice(b"syncdata");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            20,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 8,
                file_off: 0,
            },
        ),
        8
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            21,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );

    // Leave in-flight Recv, FileWrite, Timeout, and a pre-signalled waker.
    waker.wake();
    sim.buf(BufId(1))[..4].copy_from_slice(b"lost");
    sealed(|| {
        sim.submit(
            Token(100),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(2),
                off: 0,
                len: 64,
            },
        )
        .unwrap();
        sim.submit(
            Token(101),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4,
                file_off: 0,
            },
        )
        .unwrap();
        sim.submit(
            Token(102),
            Op::Timeout {
                nanos: 1_000_000_000,
            },
        )
        .unwrap();

        sim.power_loss();

        // Zero completions from pre-crash ops.
        sim.tick(false, &mut out).unwrap();
        assert!(out.is_empty());

        // Waker flag was cleared: AwaitWake waits until waker.wake() fires again.
        sim.submit(Token(200), Op::AwaitWake).unwrap();
        sim.tick(false, &mut out).unwrap();
        assert!(out.is_empty());
        waker.wake();
        sim.tick(false, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().token, Token(200));

        // Pre-crash socket is dead (-EPIPE).
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                201,
                Op::Send {
                    sock: Sock(2),
                    buf: BufId(1),
                    off: 0,
                    len: 4,
                    zero_copy: false,
                },
            ),
            -EPIPE
        );
    });

    // Rebuild both ends of a fresh connection on the same sockets (listener slot freed).
    sim.listen(Sock(0), ADDR).unwrap();
    sim.submit(
        Token(300),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    sim.submit(
        Token(301),
        Op::Connect {
            sock: Sock(2),
            addr: ADDR,
        },
    )
    .unwrap();
    let mut done = 0;
    while done < 2 {
        sim.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result, 0);
            done += 1;
        }
    }
    sim.buf(BufId(1))[..5].copy_from_slice(b"hello");
    sim.submit(
        Token(302),
        Op::Send {
            sock: Sock(2),
            buf: BufId(1),
            off: 0,
            len: 5,
            zero_copy: false,
        },
    )
    .unwrap();
    sim.submit(
        Token(303),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(2),
            off: 0,
            len: 5,
        },
    )
    .unwrap();
    done = 0;
    while done < 2 {
        sim.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result, 5);
            done += 1;
        }
    }
    assert_eq!(&sim.buf(BufId(2))[..5], b"hello");

    // Reopen file in FileSlot(0) (freed by power_loss) and verify synced data survived.
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            304,
            FileSlot(0),
            "/durable.dat",
            rw_open()
        ),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            305,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 8,
                file_off: 0,
            },
        ),
        8
    );
    assert_eq!(&sim.buf(BufId(2))[..8], b"syncdata");
}
