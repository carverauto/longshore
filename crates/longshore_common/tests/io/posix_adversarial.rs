// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Adversarial Tier 5 tests for `PosixIo` (`kqueue` / `epoll`, TCP options,
//! armed socket boundaries, Unix socket limits, UDP `RecvNow`, and `sealed`).

#![cfg(unix)]

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{
    BufId, Completions, Io, Limits, Op, Sock, Token, ECANCELED, ENFILE, ENOBUFS,
};
use std::net::SocketAddr;

#[cfg(test)]
#[path = "posix_adversarial/files_udp.rs"]
mod files_udp;

#[global_allocator]
static ALLOC: Guard = Guard;

/// Eight sockets, two of them for accepted connections, and two small buffers.
const ARMED_LIMITS: Limits = Limits::BASE
    .with_sockets_max(8)
    .with_accept_range_start(4)
    .with_accept_range_len(2)
    .with_buffers(2)
    .with_buffer_size(256)
    .with_ops_max(8);

/// Arms an accept on Sock(0) as token 1 and connects Sock(1) to it as token
/// 2; the accepted socket.
#[cfg(test)]
fn armed_accept_and_connect(io: &mut PosixIo, out: &mut Completions, addr: SocketAddr) -> Sock {
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let mut srv_armed = None;
    let mut cli_ok = false;
    while srv_armed.is_none() || !cli_ok {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 1 {
                srv_armed = Some(Sock::new(u32::try_from(c.result()).unwrap()));
            } else if c.token().get() == 2 {
                assert_eq!(c.result(), 0);
                cli_ok = true;
            }
        }
    }
    srv_armed.unwrap()
}

/// Cancels the armed accept (token 1) and waits for its final completion.
#[cfg(test)]
fn disarm_accept(io: &mut PosixIo, out: &mut Completions) {
    io.submit(
        Token::new(10),
        Op::Cancel {
            target: Token::new(1),
        },
    )
    .unwrap();
    let (mut c1, mut c10) = (false, false);
    while !c1 || !c10 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 1 && !c.more() {
                c1 = true;
            } else if c.token().get() == 10 {
                c10 = true;
            }
        }
    }
}

/// A plain accept into Sock(2) (token 3) of Sock(3)'s connect (token 4).
#[cfg(test)]
fn accept_and_connect(io: &mut PosixIo, out: &mut Completions, addr: SocketAddr) {
    io.submit(
        Token::new(3),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(4),
        Op::Connect {
            sock: Sock::new(3),
            addr,
        },
    )
    .unwrap();
    let (mut acc_ok, mut cli2_ok) = (false, false);
    while !acc_ok || !cli2_ok {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 3 {
                assert_eq!(c.result(), 0);
                acc_ok = true;
            } else if c.token().get() == 4 {
                assert_eq!(c.result(), 0);
                cli2_ok = true;
            }
        }
    }
}

#[test]
fn posix_tcp_options_inherited_and_verified_via_getsockopt() {
    let mut io = PosixIo::new(ARMED_LIMITS).unwrap();
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    let addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();

    // Verify ArmedAccept path inherits options.
    let srv_armed = armed_accept_and_connect(&mut io, &mut out, addr);
    assert_eq!(io.tcp_options(Sock::new(0)).unwrap(), (true, true));
    assert_eq!(io.tcp_options(Sock::new(1)).unwrap(), (true, true));
    assert_eq!(io.tcp_options(srv_armed).unwrap(), (true, true));

    // Disarm ArmedAccept before testing plain Op::Accept on the same listener.
    disarm_accept(&mut io, &mut out);

    // Also verify plain Op::Accept inherits options.
    accept_and_connect(&mut io, &mut out, addr);
    assert_eq!(io.tcp_options(Sock::new(2)).unwrap(), (true, true));
    assert_eq!(io.tcp_options(Sock::new(3)).unwrap(), (true, true));
}

/// Like `armed_accept_and_connect`, without checking the connect's result.
#[cfg(test)]
fn armed_accept_of_a_connect(io: &mut PosixIo, out: &mut Completions, addr: SocketAddr) -> Sock {
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let (mut srv, mut cli_ok) = (None, false);
    while srv.is_none() || !cli_ok {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 1 {
                srv = Some(Sock::new(u32::try_from(c.result()).unwrap()));
            }
            if c.token().get() == 2 {
                cli_ok = true;
            }
        }
    }
    srv.unwrap()
}

/// Arms a recv on `srv` (token 3), sends "ping" to it from Sock(1) (token
/// 4); the ring buffer the recv landed in.
#[cfg(test)]
fn armed_recv_ping(io: &mut PosixIo, out: &mut Completions, srv: Sock) -> BufId {
    io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
        .unwrap();
    io.buf(BufId::new(0))[..4].copy_from_slice(b"ping");
    io.submit(
        Token::new(4),
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    let mut got_buf = None;
    while got_buf.is_none() {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 3 {
                got_buf = c.buf();
            }
        }
    }
    got_buf.unwrap()
}

/// Closes `srv` (token 5) and the listener (token 6): both armed ops end
/// cancelled, both closes succeed.
#[cfg(test)]
fn close_ends_armed_ops(io: &mut PosixIo, out: &mut Completions, srv: Sock) {
    io.submit(Token::new(5), Op::Close { sock: srv }).unwrap();
    io.submit(Token::new(6), Op::Close { sock: Sock::new(0) })
        .unwrap();
    let mut closed = [0i32; 6];
    let mut seen = 0usize;
    while seen < 4 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            closed[(c.token().get() - 1) as usize] = c.result();
            assert!(!c.more());
            seen += 1;
        }
    }
    assert_eq!(closed[0], -ECANCELED);
    assert_eq!(closed[2], -ECANCELED);
    assert_eq!(closed[4], 0);
    assert_eq!(closed[5], 0);
}

#[test]
fn posix_sealed_armed_accept_recv_give_back_and_close_cap1() {
    let mut io =
        PosixIo::new(ARMED_LIMITS.with_ring_buffers(2).with_ring_buffer_size(128)).unwrap();
    let mut out = Completions::new(1);
    let addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();

    sealed(|| {
        let srv = armed_accept_of_a_connect(&mut io, &mut out, addr);
        let buf = armed_recv_ping(&mut io, &mut out, srv);
        io.give_back(buf);
        close_ends_armed_ops(&mut io, &mut out, srv);
    });
}

#[test]
fn posix_huge_timeout_does_not_fail_kevent_with_einval() {
    let mut io = PosixIo::new(Limits::BASE.with_sockets_max(2).with_ops_max(4)).unwrap();
    let mut out = Completions::new(4);
    let waker = io.waker().unwrap();
    io.submit(
        Token::new(1),
        Op::Timeout {
            nanos: u64::MAX / 2,
        },
    )
    .unwrap();
    io.submit(Token::new(2), Op::AwaitWake).unwrap();
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        waker.wake();
    });
    io.tick(true, &mut out).unwrap();
    h.join().unwrap();
    assert_eq!(out.pop().unwrap().token(), Token::new(2));
}

/// Connects `sock` to the Unix socket whose NUL-terminated path, `path_len`
/// bytes, is in buffer 0.
#[cfg(test)]
fn connect_unix(sock: Sock, path_len: usize) -> Op {
    Op::ConnectUnix {
        sock,
        path_buf: BufId::new(0),
        path_off: 0,
        path_len: path_len as u32,
    }
}

/// Ticks until the armed accept (token 10) fails with `-ENFILE`, 50 ticks at most.
#[cfg(test)]
fn armed_accept_fails_with_enfile(io: &mut PosixIo, out: &mut Completions) -> bool {
    let mut got_enfile = false;
    for _ in 0..50 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 10 && c.result() == -ENFILE {
                got_enfile = true;
            }
        }
        if got_enfile {
            break;
        }
    }
    got_enfile
}

/// ArmedAccept range-full (-ENFILE) on a Unix-domain listener at
/// `path_str`, with one accept slot and two connecting clients (`SO_LINGER`
/// failure ignored cleanly).
#[cfg(test)]
fn unix_armed_accept_range_full(path_str: &str, out: &mut Completions) {
    let mut io2 = PosixIo::new(
        Limits::BASE
            .with_sockets_max(4)
            .with_accept_range_start(2)
            .with_accept_range_len(1)
            .with_buffers(2)
            .with_buffer_size(256)
            .with_ops_max(8),
    )
    .unwrap();
    io2.listen_unix(Sock::new(0), path_str).unwrap();
    io2.submit(
        Token::new(10),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    let mut p = path_str.as_bytes().to_vec();
    p.push(0);
    io2.buf(BufId::new(0))[..p.len()].copy_from_slice(&p);
    io2.submit(Token::new(11), connect_unix(Sock::new(1), p.len()))
        .unwrap();
    io2.submit(Token::new(12), connect_unix(Sock::new(3), p.len()))
        .unwrap();
    assert!(armed_accept_fails_with_enfile(&mut io2, out));
}

#[test]
fn posix_armed_zero_limits_and_unix_armed_range_full_enfile() {
    let mut io = PosixIo::new(
        Limits::BASE
            .with_sockets_max(4)
            .with_buffers(1)
            .with_buffer_size(256)
            .with_ops_max(4),
    )
    .unwrap();
    let mut out = Completions::new(4);
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(Token::new(2), Op::ArmedRecv { sock: Sock::new(0) })
        .unwrap();
    io.tick(false, &mut out).unwrap();
    let mut res = Vec::new();
    while let Some(c) = out.pop() {
        res.push((c.token().get(), c.result()));
    }
    res.sort();
    assert_eq!(res, vec![(1, -ENFILE), (2, -ENOBUFS)]);

    let dir = std::env::temp_dir().join(format!("longshore_posix_adv_{}", std::process::id()));
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let sock_path = dir.join("u.sock");
    unix_armed_accept_range_full(sock_path.to_str().unwrap(), &mut out);
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
}
