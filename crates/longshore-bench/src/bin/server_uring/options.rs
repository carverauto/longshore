// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel `getsockopt` readback checks (`TCP_NODELAY` and `SO_KEEPALIVE`).

use super::checks::close_batch;
use longshore::io::posix::PosixIo;
use longshore::io::uring::UringIo;
use longshore::io::{Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

pub(super) fn check_uring_getsockopt(addr: SocketAddr) {
    let limits = Limits {
        sockets_max: 4,
        accept_range_start: 1,
        accept_range_len: 2,
        buffers: 1,
        buffer_size: 128,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).unwrap();
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    let bound = io.listen(Sock(0), addr).unwrap();
    assert_eq!(io.last_listen_tcp_options(), Some((true, true)));
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(3),
            addr: bound,
        },
    )
    .unwrap();
    let mut accepted = None;
    let mut accept_armed = true;
    let mut connected = false;
    while accepted.is_none() || !connected {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 1 {
                accept_armed = c.more;
                accepted = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token.0 == 2 {
                assert_eq!(c.result, 0);
                connected = true;
            }
        }
    }
    assert_eq!(io.last_connect_tcp_options(), Some((true, true)));
    if accept_armed {
        io.submit(Token(3), Op::Cancel { target: Token(1) })
            .unwrap();
        let (mut c_done, mut a_done) = (false, false);
        while !(c_done && a_done) {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 3 {
                    c_done = true;
                } else if c.token.0 == 1 && !c.more {
                    a_done = true;
                }
            }
        }
    }
    if let Some(opts) = io.probe_tcp_options(accepted.unwrap()).unwrap() {
        assert_eq!(opts, (true, true));
    }
    close_batch(&mut io, &mut out, &[Sock(0), Sock(3), accepted.unwrap()]);
}

pub(super) fn check_posix_getsockopt(addr: SocketAddr) {
    let limits = Limits {
        sockets_max: 4,
        accept_range_start: 1,
        accept_range_len: 2,
        buffers: 1,
        buffer_size: 128,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = PosixIo::new(limits).unwrap();
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    let bound = io.listen(Sock(0), addr).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(3),
            addr: bound,
        },
    )
    .unwrap();
    let mut accepted = None;
    let mut accept_armed = true;
    let mut connected = false;
    while accepted.is_none() || !connected {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 1 {
                accept_armed = c.more;
                accepted = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token.0 == 2 {
                assert_eq!(c.result, 0);
                connected = true;
            }
        }
    }
    assert_eq!(io.tcp_options(accepted.unwrap()).unwrap(), (true, true));
    assert_eq!(io.tcp_options(Sock(3)).unwrap(), (true, true));
    if accept_armed {
        io.submit(Token(3), Op::Cancel { target: Token(1) })
            .unwrap();
        let (mut c_done, mut a_done) = (false, false);
        while !(c_done && a_done) {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 3 {
                    c_done = true;
                } else if c.token.0 == 1 && !c.more {
                    a_done = true;
                }
            }
        }
    }
    close_batch(&mut io, &mut out, &[Sock(0), Sock(3), accepted.unwrap()]);
}
