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

#[path = "posix_adversarial/files_udp.rs"]
mod files_udp;

#[global_allocator]
static ALLOC: Guard = Guard;

#[test]
fn posix_tcp_options_inherited_and_verified_via_getsockopt() {
    let limits = Limits {
        sockets_max: 8,
        accept_range_start: 4,
        accept_range_len: 2,
        buffers: 2,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = PosixIo::new(limits).unwrap();
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();

    // Verify ArmedAccept path inherits options.
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let mut srv_armed = None;
    let mut cli_ok = false;
    while srv_armed.is_none() || !cli_ok {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(1) {
                srv_armed = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token == Token(2) {
                assert_eq!(c.result, 0);
                cli_ok = true;
            }
        }
    }
    assert_eq!(io.tcp_options(Sock(0)).unwrap(), (true, true));
    assert_eq!(io.tcp_options(Sock(1)).unwrap(), (true, true));
    assert_eq!(io.tcp_options(srv_armed.unwrap()).unwrap(), (true, true));

    // Disarm ArmedAccept before testing plain Op::Accept on the same listener.
    io.submit(Token(10), Op::Cancel { target: Token(1) })
        .unwrap();
    let (mut c1, mut c10) = (false, false);
    while !c1 || !c10 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(1) && !c.more {
                c1 = true;
            } else if c.token == Token(10) {
                c10 = true;
            }
        }
    }

    // Also verify plain Op::Accept inherits options.
    io.submit(
        Token(3),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(4),
        Op::Connect {
            sock: Sock(3),
            addr,
        },
    )
    .unwrap();
    let (mut acc_ok, mut cli2_ok) = (false, false);
    while !acc_ok || !cli2_ok {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(3) {
                assert_eq!(c.result, 0);
                acc_ok = true;
            } else if c.token == Token(4) {
                assert_eq!(c.result, 0);
                cli2_ok = true;
            }
        }
    }
    assert_eq!(io.tcp_options(Sock(2)).unwrap(), (true, true));
    assert_eq!(io.tcp_options(Sock(3)).unwrap(), (true, true));
}

#[test]
fn posix_sealed_armed_accept_recv_give_back_and_close_cap1() {
    let limits = Limits {
        sockets_max: 8,
        accept_range_start: 4,
        accept_range_len: 2,
        buffers: 2,
        buffer_size: 256,
        ring_buffers: 2,
        ring_buffer_size: 128,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = PosixIo::new(limits).unwrap();
    let mut out = Completions::new(1);
    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();

    sealed(|| {
        io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .unwrap();
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr,
            },
        )
        .unwrap();
        let (mut srv, mut cli_ok) = (None, false);
        while srv.is_none() || !cli_ok {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == Token(1) {
                    srv = Some(Sock(u32::try_from(c.result).unwrap()));
                }
                if c.token == Token(2) {
                    cli_ok = true;
                }
            }
        }
        let srv = srv.unwrap();
        io.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();
        io.buf(BufId(0))[..4].copy_from_slice(b"ping");
        io.submit(
            Token(4),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 4,
                zero_copy: false,
            },
        )
        .unwrap();
        let mut got_buf = None;
        while got_buf.is_none() {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == Token(3) {
                    got_buf = c.buf;
                }
            }
        }
        io.give_back(got_buf.unwrap());
        io.submit(Token(5), Op::Close { sock: srv }).unwrap();
        io.submit(Token(6), Op::Close { sock: Sock(0) }).unwrap();
        let mut closed = [0i32; 6];
        let mut seen = 0usize;
        while seen < 4 {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                closed[(c.token.0 - 1) as usize] = c.result;
                assert!(!c.more);
                seen += 1;
            }
        }
        assert_eq!(closed[0], -ECANCELED);
        assert_eq!(closed[2], -ECANCELED);
        assert_eq!(closed[4], 0);
        assert_eq!(closed[5], 0);
    });
}

#[test]
fn posix_huge_timeout_does_not_fail_kevent_with_einval() {
    let mut io = PosixIo::new(Limits {
        sockets_max: 2,
        ops_max: 4,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(4);
    let waker = io.waker().unwrap();
    io.submit(
        Token(1),
        Op::Timeout {
            nanos: u64::MAX / 2,
        },
    )
    .unwrap();
    io.submit(Token(2), Op::AwaitWake).unwrap();
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        waker.wake();
    });
    io.tick(true, &mut out).unwrap();
    h.join().unwrap();
    assert_eq!(out.pop().unwrap().token, Token(2));
}

#[test]
fn posix_armed_zero_limits_and_unix_armed_range_full_enfile() {
    let mut io = PosixIo::new(Limits {
        sockets_max: 4,
        buffers: 1,
        buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(4);
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(Token(2), Op::ArmedRecv { sock: Sock(0) })
        .unwrap();
    io.tick(false, &mut out).unwrap();
    let mut res = Vec::new();
    while let Some(c) = out.pop() {
        res.push((c.token.0, c.result));
    }
    res.sort();
    assert_eq!(res, vec![(1, -ENFILE), (2, -ENOBUFS)]);

    // ArmedAccept range-full (-ENFILE) on Unix-domain listener (SO_LINGER failure ignored cleanly).
    let dir = std::env::temp_dir().join(format!("longshore_posix_adv_{}", std::process::id()));
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let sock_path = dir.join("u.sock");
    let path_str = sock_path.to_str().unwrap();

    let mut io2 = PosixIo::new(Limits {
        sockets_max: 4,
        accept_range_start: 2,
        accept_range_len: 1,
        buffers: 2,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    })
    .unwrap();
    io2.listen_unix(Sock(0), path_str).unwrap();
    io2.submit(Token(10), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    let mut p = path_str.as_bytes().to_vec();
    p.push(0);
    io2.buf(BufId(0))[..p.len()].copy_from_slice(&p);
    io2.submit(
        Token(11),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: p.len() as u32,
        },
    )
    .unwrap();
    io2.submit(
        Token(12),
        Op::ConnectUnix {
            sock: Sock(3),
            path_buf: BufId(0),
            path_off: 0,
            path_len: p.len() as u32,
        },
    )
    .unwrap();
    let mut got_enfile = false;
    for _ in 0..50 {
        io2.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(10) && c.result == -ENFILE {
                got_enfile = true;
            }
        }
        if got_enfile {
            break;
        }
    }
    assert!(got_enfile);
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
}
