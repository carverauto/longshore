// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Whole-machine `power_loss` tests: dropping in-flight ops, freeing sockets
//! and file slots, clearing timers/waker, and rebuilding both ends of a stream.

use super::{drive, make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op, Sock, Token, EPIPE};
use longshore_common::sim::SimIo;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9000);

#[test]
fn power_loss_drops_in_flight_ops_frees_slots_and_rebuilds_callers() {
    let mut sim = make_sim(19, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    let waker = sim.waker().unwrap();

    let _: [i32; 2] = connect_pair(&mut sim, &mut out, 1);
    write_synced_file(&mut sim, &mut out);

    // Leave in-flight Recv, FileWrite, Timeout, and a pre-signalled waker.
    waker.wake();
    sim.buf(BufId::new(1))[..4].copy_from_slice(b"lost");
    sealed(|| {
        submit_in_flight_ops(&mut sim);

        sim.power_loss();

        // Zero completions from pre-crash ops.
        sim.tick(false, &mut out).unwrap();
        assert!(out.is_empty());

        // Waker flag was cleared: AwaitWake waits until waker.wake() fires again.
        sim.submit(Token::new(200), Op::AwaitWake).unwrap();
        sim.tick(false, &mut out).unwrap();
        assert!(out.is_empty());
        waker.wake();
        sim.tick(false, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().token(), Token::new(200));

        // Pre-crash socket is dead (-EPIPE).
        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                201,
                Op::Send {
                    sock: Sock::new(2),
                    buf: BufId::new(1),
                    off: 0,
                    len: 4,
                    zero_copy: false,
                },
            ),
            -EPIPE
        );
    });

    // Rebuild both ends of a fresh connection on the same sockets (listener slot freed).
    assert_eq!(connect_pair(&mut sim, &mut out, 300), [0, 0]);
    send_and_receive_hello(&mut sim, &mut out);

    // Reopen file in FileSlot(0) (freed by power_loss) and verify synced data survived.
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            304,
            FileSlot::new(0),
            "/durable.dat",
            rw_open()
        ),
        0
    );
    assert_eq!(read_at(&mut sim, &mut out, 305, FileSlot::new(0), 0, 8), 8);
    assert_eq!(&sim.buf(BufId::new(2))[..8], b"syncdata");
}

/// Listens on `Sock(0)`, accepts into `Sock(1)` (token `tok`) and connects `Sock(2)`
/// (token `tok + 1`); returns both results in completion order.
#[cfg(test)]
fn connect_pair(sim: &mut SimIo, out: &mut Completions, tok: u64) -> [i32; 2] {
    let _: SocketAddr = sim.listen(Sock::new(0), ADDR).unwrap();
    sim.submit(
        Token::new(tok),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .unwrap();
    sim.submit(
        Token::new(tok + 1),
        Op::Connect {
            sock: Sock::new(2),
            addr: ADDR,
        },
    )
    .unwrap();
    collect_two(sim, out)
}

/// Ticks until two completions arrive; returns their results in order.
#[cfg(test)]
fn collect_two(sim: &mut SimIo, out: &mut Completions) -> [i32; 2] {
    let mut results = [0; 2];
    let mut done = 0;
    while done < 2 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if let Some(r) = results.get_mut(done) {
                *r = c.result();
            }
            done += 1;
        }
    }
    results
}

/// Creates `/durable.dat` with a durable entry and the synced contents `syncdata`.
fn write_synced_file(sim: &mut SimIo, out: &mut Completions) {
    assert_eq!(
        open_path(sim, out, 3, FileSlot::new(0), "/durable.dat", rw_create()),
        0
    );
    sync_dir_path(sim, out, 10, "/");
    sim.buf(BufId::new(1))[..8].copy_from_slice(b"syncdata");
    assert_eq!(
        drive(
            sim,
            out,
            20,
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 8,
                file_off: 0,
            },
        ),
        8
    );
    assert_eq!(sync_slot(sim, out, 21, FileSlot::new(0)), 0);
}

/// A `Recv`, a `FileWrite` and a `Timeout` that power loss must drop (tokens 100..103).
#[cfg(test)]
fn submit_in_flight_ops(sim: &mut SimIo) {
    sim.submit(
        Token::new(100),
        Op::Recv {
            sock: Sock::new(1),
            buf: BufId::new(2),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    sim.submit(
        Token::new(101),
        Op::FileWrite {
            slot: FileSlot::new(0),
            buf: BufId::new(1),
            off: 0,
            len: 4,
            file_off: 0,
        },
    )
    .unwrap();
    sim.submit(
        Token::new(102),
        Op::Timeout {
            nanos: 1_000_000_000,
        },
    )
    .unwrap();
}

/// `hello` goes from `Sock(2)` to `Sock(1)` (tokens 302, 303).
#[cfg(test)]
fn send_and_receive_hello(sim: &mut SimIo, out: &mut Completions) {
    sim.buf(BufId::new(1))[..5].copy_from_slice(b"hello");
    sim.submit(
        Token::new(302),
        Op::Send {
            sock: Sock::new(2),
            buf: BufId::new(1),
            off: 0,
            len: 5,
            zero_copy: false,
        },
    )
    .unwrap();
    sim.submit(
        Token::new(303),
        Op::Recv {
            sock: Sock::new(1),
            buf: BufId::new(2),
            off: 0,
            len: 5,
        },
    )
    .unwrap();
    assert_eq!(collect_two(sim, out), [5, 5]);
    assert_eq!(&sim.buf(BufId::new(2))[..5], b"hello");
}
