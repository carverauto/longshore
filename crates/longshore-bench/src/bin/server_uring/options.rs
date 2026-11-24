// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel `getsockopt` readback checks (`TCP_NODELAY` and `SO_KEEPALIVE`).

use super::checks::close_batch;
use crate::die::OrDie;
use longshore::io::posix::PosixIo;
use longshore::io::uring::UringIo;
use longshore::io::{Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

const LISTENER: Sock = Sock::new(0);
const CLIENT: Sock = Sock::new(3);
const ACCEPT: Token = Token::new(1);

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_accept_range_start(1)
    .with_accept_range_len(2)
    .with_buffers(1)
    .with_buffer_size(128)
    .with_ops_max(8);

/// Arm an accept on the listener at `bound` and connect the client to it.
/// Returns the accepted socket and whether the armed accept is still armed.
fn accept_and_connect<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    bound: SocketAddr,
) -> (Sock, bool) {
    io.submit(ACCEPT, Op::ArmedAccept { listener: LISTENER })
        .or_die("submit");
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: CLIENT,
            addr: bound,
        },
    )
    .or_die("submit");
    let mut accepted = None;
    let mut accept_armed = true;
    let mut connected = false;
    while accepted.is_none() || !connected {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token() == ACCEPT {
                accept_armed = c.more();
                accepted = Some(Sock::new(
                    u32::try_from(c.result()).or_die("accepted socket"),
                ));
            } else if c.token().get() == 2 {
                assert_eq!(c.result(), 0);
                connected = true;
            }
        }
    }
    (accepted.or_die("an accepted socket"), accept_armed)
}

/// Cancel the armed accept and wait for both the cancel and its last completion.
fn cancel_accept<IO: Io>(io: &mut IO, out: &mut Completions) {
    io.submit(Token::new(3), Op::Cancel { target: ACCEPT })
        .or_die("submit");
    let (mut c_done, mut a_done) = (false, false);
    while !(c_done && a_done) {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == 3 {
                c_done = true;
            } else if c.token() == ACCEPT && !c.more() {
                a_done = true;
            }
        }
    }
}

pub(super) fn check_uring_getsockopt(addr: SocketAddr) {
    let mut io = UringIo::new(LIMITS).or_die("UringIo::new");
    io.no_delay(true);
    io.keep_alive(true);
    io.keep_alive_timing(Some(longshore::io::KeepAlive::new(60, 10, 6)));
    let mut out = Completions::new(8);
    let bound = io.listen(LISTENER, addr).or_die("listen");
    assert_eq!(io.last_listen_tcp_options(), Some((true, true)));
    assert_eq!(io.last_listen_keep_timing(), Some((60, 10, 6)));
    let (accepted, accept_armed) = accept_and_connect(&mut io, &mut out, bound);
    assert_eq!(io.last_connect_tcp_options(), Some((true, true)));
    assert_eq!(io.last_connect_keep_timing(), Some((60, 10, 6)));
    if accept_armed {
        cancel_accept(&mut io, &mut out);
    }
    if let Some(opts) = io.probe_tcp_options(accepted).or_die("probe_tcp_options") {
        assert_eq!(opts, (true, true));
    }
    close_batch(&mut io, &mut out, &[LISTENER, CLIENT, accepted]);
}

pub(super) fn check_posix_getsockopt(addr: SocketAddr) {
    let mut io = PosixIo::new(LIMITS).or_die("PosixIo::new");
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(8);
    let bound = io.listen(LISTENER, addr).or_die("listen");
    let (accepted, accept_armed) = accept_and_connect(&mut io, &mut out, bound);
    assert_eq!(io.tcp_options(accepted).or_die("tcp_options"), (true, true));
    assert_eq!(io.tcp_options(CLIENT).or_die("tcp_options"), (true, true));
    if accept_armed {
        cancel_accept(&mut io, &mut out);
    }
    close_batch(&mut io, &mut out, &[LISTENER, CLIENT, accepted]);
}
