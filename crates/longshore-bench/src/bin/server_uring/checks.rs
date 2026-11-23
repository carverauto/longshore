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
use longshore::io::{contract_net, BufId, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

pub(super) fn close_batch<IO: Io>(io: &mut IO, out: &mut Completions, socks: &[Sock]) {
    for chunk in socks.chunks(128) {
        for (k, &s) in chunk.iter().enumerate() {
            io.submit(Token(90_000 + k as u64), Op::Close { sock: s })
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

fn peer_sees_eof_on_armed_close<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = Limits {
        sockets_max: 4,
        accept_range_start: 1,
        accept_range_len: 2,
        buffers: 1,
        buffer_size: 128,
        ring_buffers: 2,
        ring_buffer_size: 128,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = make(limits);
    let mut out = Completions::new(8);
    let bound = io.listen(Sock(0), addr).or_die("listen");
    sealed(|| {
        io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .or_die("submit");
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(3),
                addr: bound,
            },
        )
        .or_die("submit");
        let mut srv = None;
        let mut accept_armed = true;
        let mut conn = false;
        while srv.is_none() || !conn {
            io.tick(true, &mut out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token.0 == 1 {
                    accept_armed = c.more;
                    srv = Some(Sock(u32::try_from(c.result).or_die("accepted socket")));
                } else if c.token.0 == 2 {
                    assert_eq!(c.result, 0);
                    conn = true;
                }
            }
        }
        let srv = srv.or_die("an accepted socket");
        if accept_armed {
            io.submit(Token(99), Op::Cancel { target: Token(1) })
                .or_die("submit");
            let (mut c_ok, mut a_end) = (false, false);
            while !(c_ok && a_end) {
                io.tick(true, &mut out).or_die("tick");
                while let Some(c) = out.pop() {
                    if c.token.0 == 99 {
                        c_ok = true;
                    } else if c.token.0 == 1 && !c.more {
                        a_end = true;
                    }
                }
            }
        }
        io.submit(Token(3), Op::ArmedRecv { sock: srv })
            .or_die("submit");
        io.tick(false, &mut out).or_die("tick");
        assert!(out.is_empty());

        io.submit(Token(4), Op::Close { sock: srv })
            .or_die("submit");
        let (mut r_done, mut c_done) = (false, false);
        while !(r_done && c_done) {
            io.tick(true, &mut out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token.0 == 3 {
                    assert!(!c.more && c.buf.is_none() && c.result < 0, "{c:?}");
                    r_done = true;
                } else if c.token.0 == 4 {
                    assert_eq!(c.result, 0);
                    c_done = true;
                }
            }
        }
        io.submit(
            Token(5),
            Op::Recv {
                sock: Sock(3),
                buf: BufId(0),
                off: 0,
                len: 64,
            },
        )
        .or_die("submit");
        loop {
            io.tick(true, &mut out).or_die("tick");
            if let Some(c) = out.pop() {
                if c.token.0 == 5 {
                    assert_eq!(c.result, 0);
                    break;
                }
            }
        }
        close_batch(&mut io, &mut out, &[Sock(0), Sock(3)]);
    });
}

fn cq_driven_to_limit<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    let limits = Limits {
        sockets_max: 8,
        accept_range_start: 1,
        accept_range_len: 4,
        buffers: 2,
        buffer_size: 256,
        ring_buffers: 8,
        ring_buffer_size: 64,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = make(limits);
    let mut out = Completions::new(2);
    let bound = io.listen(Sock(0), addr).or_die("listen");
    sealed(|| {
        io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .or_die("submit");
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(5),
                addr: bound,
            },
        )
        .or_die("submit");
        let mut srv = None;
        let mut accept_armed = true;
        let mut conn = false;
        while srv.is_none() || !conn {
            io.tick(true, &mut out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token.0 == 1 {
                    accept_armed = c.more;
                    srv = Some(Sock(u32::try_from(c.result).or_die("accepted socket")));
                } else if c.token.0 == 2 {
                    conn = true;
                }
            }
        }
        let srv = srv.or_die("an accepted socket");
        if accept_armed {
            io.submit(Token(99), Op::Cancel { target: Token(1) })
                .or_die("submit");
            let (mut c_ok, mut a_end) = (false, false);
            while !(c_ok && a_end) {
                io.tick(true, &mut out).or_die("tick");
                while let Some(c) = out.pop() {
                    if c.token.0 == 99 {
                        c_ok = true;
                    } else if c.token.0 == 1 && !c.more {
                        a_end = true;
                    }
                }
            }
        }
        io.submit(Token(3), Op::ArmedRecv { sock: srv })
            .or_die("submit");
        let mut recv_armed = true;

        let mut total_rx = 0usize;
        for round in 0..40u64 {
            io.buf(BufId(0))[..16].fill(round as u8);
            io.submit(
                Token(100 + round),
                Op::Send {
                    sock: Sock(5),
                    buf: BufId(0),
                    off: 0,
                    len: 16,
                    zero_copy: false,
                },
            )
            .or_die("submit");
            io.submit(Token(200 + round), Op::Timeout { nanos: 0 })
                .or_die("submit");
            let (mut s_ok, mut t_ok) = (false, false);
            while !(s_ok && t_ok) {
                io.tick(true, &mut out).or_die("tick");
                while let Some(c) = out.pop() {
                    if c.token.0 == 100 + round {
                        assert_eq!(c.result, 16);
                        s_ok = true;
                    } else if c.token.0 == 200 + round {
                        assert_eq!(c.result, 0);
                        t_ok = true;
                    } else if c.token.0 == 3 {
                        recv_armed = c.more;
                        if c.result > 0 {
                            total_rx += usize::try_from(c.result).or_die("received bytes");
                            io.give_back(c.buf.or_die("a ring buffer in the completion"));
                        }
                        if !recv_armed && total_rx < (round as usize + 1) * 16 {
                            io.submit(Token(3), Op::ArmedRecv { sock: srv })
                                .or_die("submit");
                            recv_armed = true;
                        }
                    }
                }
            }
        }
        while total_rx < 40 * 16 {
            if !recv_armed {
                io.submit(Token(3), Op::ArmedRecv { sock: srv })
                    .or_die("submit");
                recv_armed = true;
            }
            io.tick(true, &mut out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token.0 == 3 {
                    recv_armed = c.more;
                    if c.result > 0 {
                        total_rx += usize::try_from(c.result).or_die("received bytes");
                        io.give_back(c.buf.or_die("a ring buffer in the completion"));
                    }
                }
            }
        }
        if recv_armed {
            io.submit(Token(98), Op::Cancel { target: Token(3) })
                .or_die("submit");
            let (mut c_ok, mut r_end) = (false, false);
            while !(c_ok && r_end) {
                io.tick(true, &mut out).or_die("tick");
                while let Some(c) = out.pop() {
                    if c.token.0 == 98 {
                        c_ok = true;
                    } else if c.token.0 == 3 && !c.more {
                        r_end = true;
                    }
                }
            }
        }
        close_batch(&mut io, &mut out, &[Sock(0), Sock(5), srv]);
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
