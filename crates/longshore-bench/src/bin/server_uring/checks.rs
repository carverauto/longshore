// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel checks executed by `server_uring` on `UringIo` and `PosixIo`.

use super::options::{check_posix_getsockopt, check_uring_getsockopt};
use super::thousand::thousand_idle_connections;
use crate::die::OrDie;
use longshore::io::alloc_guard::sealed;
use longshore::io::posix::PosixIo;
use longshore::io::uring::UringIo;
use longshore::io::{contract_net, BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

pub(super) fn close_batch<IO: Io>(io: &mut IO, out: &mut Completions, socks: &[Sock]) {
    for chunk in socks.chunks(128) {
        for (k, &s) in chunk.iter().enumerate() {
            io.submit(Token::new(90_000 + k as u64), Op::Close { sock: s })
                .or_die("submit");
        }
        let mut left = chunk.len();
        while left > 0 {
            io.tick(true, out).or_die("tick");
            while out.pop().is_some() {
                left -= 1;
            }
        }
    }
}

/// Arm an accept on listener `Sock(0)`, connect `client` to `bound`, and
/// return the accepted socket with the connect's result. The armed accept
/// is cancelled (and its end awaited) if it is still armed.
fn accept_one<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    bound: SocketAddr,
    client: Sock,
) -> (Sock, i32) {
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .or_die("submit");
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: client,
            addr: bound,
        },
    )
    .or_die("submit");
    let mut srv = None;
    let mut accept_armed = true;
    let mut conn = None;
    while srv.is_none() || conn.is_none() {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == 1 {
                accept_armed = c.more();
                srv = Some(Sock::new(
                    u32::try_from(c.result()).or_die("accepted socket"),
                ));
            } else if c.token().get() == 2 {
                conn = Some(c.result());
            }
        }
    }
    let srv = srv.or_die("an accepted socket");
    if accept_armed {
        cancel_armed(io, out, Token::new(99), Token::new(1));
    }
    (srv, conn.or_die("a connect result"))
}

/// Cancel the armed operation `target` with token `cancel`, and wait for
/// both the cancel and the armed operation's last completion.
pub(super) fn cancel_armed<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    cancel: Token,
    target: Token,
) {
    io.submit(cancel, Op::Cancel { target }).or_die("submit");
    let (mut c_ok, mut a_end) = (false, false);
    while !(c_ok && a_end) {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token() == cancel {
                c_ok = true;
            } else if c.token() == target && !c.more() {
                a_end = true;
            }
        }
    }
}

fn peer_sees_eof_on_armed_close<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_accept_range_start(1)
        .with_accept_range_len(2)
        .with_buffers(1)
        .with_buffer_size(128)
        .with_ring_buffers(2)
        .with_ring_buffer_size(128)
        .with_ops_max(8);
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let bound = io.listen(Sock::new(0), addr).or_die("listen");
    sealed(|| {
        let (srv, connected) = accept_one(&mut io, &mut out, bound, Sock::new(3));
        assert_eq!(connected, 0);
        io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
            .or_die("submit");
        io.tick(false, &mut out).or_die("tick");
        assert!(out.is_empty());

        io.submit(Token::new(4), Op::Close { sock: srv })
            .or_die("submit");
        let (mut r_done, mut c_done) = (false, false);
        while !(r_done && c_done) {
            io.tick(true, &mut out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token().get() == 3 {
                    assert!(!c.more() && c.buf().is_none() && c.result() < 0, "{c:?}");
                    r_done = true;
                } else if c.token().get() == 4 {
                    assert_eq!(c.result(), 0);
                    c_done = true;
                }
            }
        }
        io.submit(
            Token::new(5),
            Op::Recv {
                sock: Sock::new(3),
                buf: BufId::new(0),
                off: 0,
                len: 64,
            },
        )
        .or_die("submit");
        loop {
            io.tick(true, &mut out).or_die("tick");
            if let Some(c) = out.pop() {
                if c.token().get() == 5 {
                    assert_eq!(c.result(), 0);
                    break;
                }
            }
        }
        close_batch(&mut io, &mut out, &[Sock::new(0), Sock::new(3)]);
    });
}

/// Count a completion of the armed receive (token 3) on `srv`: add its
/// bytes to `total_rx`, return its ring buffer, and note whether it is
/// still armed.
fn on_recv<IO: Io>(io: &mut IO, c: &Completion, total_rx: &mut usize, recv_armed: &mut bool) {
    *recv_armed = c.more();
    if c.result() > 0 {
        *total_rx += usize::try_from(c.result()).or_die("received bytes");
        io.give_back(c.buf().or_die("a ring buffer in the completion"));
    }
}

/// Send 40 rounds of 16 bytes from `Sock(5)` to `srv`, each with a zero
/// timeout, re-arming the receive whenever it ends early. Returns the
/// bytes received so far and whether the receive is still armed.
fn send_rounds<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock) -> (usize, bool) {
    let mut recv_armed = true;
    let mut total_rx = 0usize;
    for round in 0..40u64 {
        io.buf(BufId::new(0))[..16].fill(round as u8);
        io.submit(
            Token::new(100 + round),
            Op::Send {
                sock: Sock::new(5),
                buf: BufId::new(0),
                off: 0,
                len: 16,
                zero_copy: false,
            },
        )
        .or_die("submit");
        io.submit(Token::new(200 + round), Op::Timeout { nanos: 0 })
            .or_die("submit");
        let (mut s_ok, mut t_ok) = (false, false);
        while !(s_ok && t_ok) {
            io.tick(true, out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token().get() == 100 + round {
                    assert_eq!(c.result(), 16);
                    s_ok = true;
                } else if c.token().get() == 200 + round {
                    assert_eq!(c.result(), 0);
                    t_ok = true;
                } else if c.token().get() == 3 {
                    on_recv(io, &c, &mut total_rx, &mut recv_armed);
                    if !recv_armed && total_rx < (round as usize + 1) * 16 {
                        io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
                            .or_die("submit");
                        recv_armed = true;
                    }
                }
            }
        }
    }
    (total_rx, recv_armed)
}

fn cq_driven_to_limit<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = Limits::BASE
        .with_sockets_max(8)
        .with_accept_range_start(1)
        .with_accept_range_len(4)
        .with_buffers(2)
        .with_buffer_size(256)
        .with_ring_buffers(8)
        .with_ring_buffer_size(64)
        .with_ops_max(8);
    let mut io = make(limits);
    let mut out = Completions::new(2);
    let bound = io.listen(Sock::new(0), addr).or_die("listen");
    sealed(|| {
        let (srv, _): (Sock, i32) = accept_one(&mut io, &mut out, bound, Sock::new(5));
        io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
            .or_die("submit");
        let (mut total_rx, mut recv_armed) = send_rounds(&mut io, &mut out, srv);
        while total_rx < 40 * 16 {
            if !recv_armed {
                io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
                    .or_die("submit");
                recv_armed = true;
            }
            io.tick(true, &mut out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token().get() == 3 {
                    on_recv(&mut io, &c, &mut total_rx, &mut recv_armed);
                }
            }
        }
        if recv_armed {
            cancel_armed(&mut io, &mut out, Token::new(98), Token::new(3));
        }
        close_batch(&mut io, &mut out, &[Sock::new(0), Sock::new(5), srv]);
    });
}

pub(super) fn run_uring(addr: SocketAddr) {
    let unix_path = format!("/tmp/longshore-server-uring-{}.sock", std::process::id());
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_file(&unix_path).ok();
    contract_net::run_all(
        &mut |l| UringIo::new(l).or_die("UringIo::new"),
        addr,
        &unix_path,
    );
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_file(&unix_path).ok();

    thousand_idle_connections(&mut |l| UringIo::new(l).or_die("UringIo::new"), addr);
    peer_sees_eof_on_armed_close(&mut |l| UringIo::new(l).or_die("UringIo::new"), addr);
    cq_driven_to_limit(&mut |l| UringIo::new(l).or_die("UringIo::new"), addr);
    check_uring_getsockopt(addr);
}

pub(super) fn run_posix(addr: SocketAddr) {
    let unix_path = format!("/tmp/longshore-server-posix-{}.sock", std::process::id());
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_file(&unix_path).ok();
    contract_net::run_all(
        &mut |l| PosixIo::new(l).or_die("PosixIo::new"),
        addr,
        &unix_path,
    );
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_file(&unix_path).ok();

    thousand_idle_connections(&mut |l| PosixIo::new(l).or_die("PosixIo::new"), addr);
    peer_sees_eof_on_armed_close(&mut |l| PosixIo::new(l).or_die("PosixIo::new"), addr);
    cq_driven_to_limit(&mut |l| PosixIo::new(l).or_die("PosixIo::new"), addr);
    check_posix_getsockopt(addr);
}
