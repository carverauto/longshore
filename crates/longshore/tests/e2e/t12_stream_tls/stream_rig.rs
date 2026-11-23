// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The Feature 18 stream rig: a connected simulated socket pair and a driver
//! for client and server TLS handshakes over it.

#[cfg(feature = "sim")]
use super::super::harness::drive_until;
#[cfg(feature = "sim")]
use longshore::io::{Completions, Io, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::SimIo;
#[cfg(all(feature = "sim", feature = "tls"))]
use longshore::stream::{Step, Stream};
#[cfg(all(feature = "sim", feature = "tls"))]
use longshore::tls::handshake::Handshake;
#[cfg(feature = "sim")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(feature = "sim")]
pub(super) const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 18)), 8443);
#[cfg(feature = "sim")]
pub(super) const LISTENER: Sock = Sock(0);
#[cfg(feature = "sim")]
pub(super) const CLIENT: Sock = Sock(1);
#[cfg(feature = "sim")]
pub(super) const SERVER: Sock = Sock(2);

#[cfg(feature = "sim")]
pub(super) fn connect_pair_sim(io: &mut SimIo, out: &mut Completions) {
    let addr = io.listen(LISTENER, ADDR).expect("listen");
    io.submit(
        Token(10),
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )
    .unwrap();
    io.submit(Token(11), Op::Connect { sock: CLIENT, addr })
        .unwrap();
    let cs = drive_until(io, out, 100, |v| {
        v.iter().any(|c| c.token == Token(10)) && v.iter().any(|c| c.token == Token(11))
    })
    .expect("pair connect");
    for c in cs {
        assert!(c.result >= 0, "connect/accept failed: {c:?}");
    }
}

#[cfg(all(feature = "sim", feature = "tls"))]
pub(super) fn drive_tls_handshakes(
    io: &mut SimIo,
    out: &mut Completions,
    c_stream: &mut Stream,
    s_stream: &mut Stream,
    c_hs: Handshake,
    s_hs: Handshake,
) -> (Step, Step) {
    let mut c_step = c_stream.start_handshake(io, c_hs);
    let mut s_step = s_stream.start_handshake(io, s_hs);
    for _ in 0..200 {
        if (!matches!(c_step, Step::Pending) && !matches!(s_step, Step::Pending))
            || matches!(c_step, Step::Tls(_))
            || matches!(s_step, Step::Tls(_))
        {
            break;
        }
        io.tick(true, out).expect("tick");
        while let Some(c) = out.pop() {
            match c.token.0 {
                1 => c_step = c_stream.on_handshake(io, true, c.result),
                2 => c_step = c_stream.on_handshake(io, false, c.result),
                3 => s_step = s_stream.on_handshake(io, true, c.result),
                4 => s_step = s_stream.on_handshake(io, false, c.result),
                _ => {}
            }
        }
    }
    (c_step, s_step)
}
