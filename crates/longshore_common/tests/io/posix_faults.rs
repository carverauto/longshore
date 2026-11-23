// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Seeded stale-readiness tests for `PosixIo` under `fault-injection`.

#![cfg(all(unix, feature = "fault-injection"))]

use longshore_common::io::posix::PosixIo;
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(8)
    .with_buffers(4)
    .with_buffer_size(4096)
    .with_ops_max(8);

fn v4() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

#[cfg(test)]
fn connect_pair(
    io: &mut PosixIo,
    out: &mut Completions,
    listener: Sock,
    server: Sock,
    client: Sock,
) {
    let addr = io.listen(listener, v4()).unwrap();
    io.submit(
        Token::new(1),
        Op::Accept {
            listener,
            sock: server,
        },
    )
    .unwrap();
    io.submit(Token::new(2), Op::Connect { sock: client, addr })
        .unwrap();
    let mut done = 0;
    while done < 2 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result(), 0);
            done += 1;
        }
    }
}

#[cfg(test)]
fn recv64(sock: Sock, buf: u32) -> Op {
    Op::Recv {
        sock,
        buf: BufId::new(buf),
        off: 0,
        len: 64,
    }
}

#[cfg(test)]
fn send(sock: Sock, buf: u32, len: u32) -> Op {
    Op::Send {
        sock,
        buf: BufId::new(buf),
        off: 0,
        len,
        zero_copy: false,
    }
}

/// Sends "real" from Sock(4) to Sock(5), injects a stale read report on the
/// idle Sock(2) once it is delivered: the recv on Sock(5) (token 11)
/// completes, the one on Sock(2) (token 10) does not.
#[cfg(test)]
fn real_data_completes_despite_stale_report(io: &mut PosixIo, out: &mut Completions, seed: u64) {
    io.buf(BufId::new(2))[..4].copy_from_slice(b"real");
    io.submit(Token::new(12), send(Sock::new(4), 2, 4)).unwrap();
    // Wait for Send and delivery, then inject stale on Sock(2).
    io.tick(true, out).unwrap();
    let mut got_real = false;
    while let Some(c) = out.pop() {
        assert_ne!(
            c.token().get(),
            10,
            "seed {seed}: idle socket must not complete"
        );
        if c.token().get() == 11 {
            got_real = true;
        }
    }
    io.inject_stale(Sock::new(2), false);
    while !got_real {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            assert_ne!(
                c.token().get(),
                10,
                "seed {seed}: stale report must not complete"
            );
            if c.token().get() == 11 {
                assert_eq!(c.result(), 4);
                got_real = true;
            }
        }
    }
}

/// Sends "end" from Sock(1) to the idle Sock(2): its recv (token 10) still
/// completes normally.
#[cfg(test)]
fn idle_socket_completes_on_real_data(io: &mut PosixIo, out: &mut Completions) {
    io.buf(BufId::new(3))[..3].copy_from_slice(b"end");
    io.submit(Token::new(13), send(Sock::new(1), 3, 3)).unwrap();
    let mut got_idle = false;
    while !got_idle {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 10 {
                assert_eq!(c.result(), 3);
                got_idle = true;
            }
        }
    }
}

#[test]
fn stale_report_among_real_reports_completes_real_ops() {
    for seed in 0..8u64 {
        let mut io = PosixIo::new(LIMITS).unwrap();
        let mut out = Completions::new(8);
        connect_pair(&mut io, &mut out, Sock::new(0), Sock::new(1), Sock::new(2));
        connect_pair(&mut io, &mut out, Sock::new(3), Sock::new(4), Sock::new(5));

        // Idle socket Sock(2) gets a spurious read report; Sock(5) gets real data.
        io.submit(Token::new(10), recv64(Sock::new(2), 0)).unwrap();
        io.submit(Token::new(11), recv64(Sock::new(5), 1)).unwrap();
        io.tick(false, &mut out).unwrap();
        real_data_completes_despite_stale_report(&mut io, &mut out, seed);

        // Now send real data to Sock(2); it must still complete normally.
        idle_socket_completes_on_real_data(&mut io, &mut out);
    }
}

#[test]
fn stale_report_alone_waits_for_real_data() {
    use std::io::Write;
    for seed in 0..8u64 {
        let listener = TcpListener::bind(v4()).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut io = PosixIo::new(LIMITS).unwrap();
        let mut out = Completions::new(8);

        let h = std::thread::spawn(move || listener.accept().unwrap().0);
        io.submit(
            Token::new(1),
            Op::Connect {
                sock: Sock::new(0),
                addr,
            },
        )
        .unwrap();
        loop {
            io.tick(true, &mut out).unwrap();
            if let Some(c) = out.pop() {
                assert_eq!((c.token().get(), c.result()), (1, 0), "seed {seed}");
                break;
            }
        }
        let mut peer = h.join().unwrap();

        io.submit(
            Token::new(2),
            Op::Recv {
                sock: Sock::new(0),
                buf: BufId::new(0),
                off: 0,
                len: 32,
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();
        assert!(out.is_empty());

        io.inject_stale(Sock::new(0), false);
        io.tick(false, &mut out).unwrap();
        assert!(
            out.is_empty(),
            "seed {seed}: stale report alone must yield no completion"
        );

        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            peer.write_all(b"later").unwrap();
        });
        io.tick(true, &mut out).unwrap();
        let c = out.pop().expect("real completion after data arrives");
        assert_eq!((c.token().get(), c.result()), (2, 5), "seed {seed}");
        assert_eq!(&io.buf(BufId::new(0))[..5], b"later");
        writer.join().unwrap();
    }
}
