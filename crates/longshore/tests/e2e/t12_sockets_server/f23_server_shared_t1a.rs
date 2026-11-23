// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 23 Tier 1 (part A) E2E tests: shared server networking suite & sim faults.

#[cfg(unix)]
use super::super::harness::posix_backend;
#[cfg(any(feature = "sim", unix))]
use super::super::harness::{
    drive_until, read_pool_bytes, write_nul_path, write_pool_bytes, TempDir,
};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(any(feature = "sim", unix))]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(any(feature = "sim", unix))]
use std::net::SocketAddr;

#[cfg(any(feature = "sim", unix))]
pub(super) fn server_limits() -> Limits {
    Limits {
        sockets_max: 24,
        buffers: 4,
        buffer_size: 2048,
        ops_max: 24,
        accept_range_start: 12,
        accept_range_len: 8,
        ring_buffers: 8,
        ring_buffer_size: 1024,
        ..Limits::BASE
    }
}

/// Armed accept and armed recv over TCP, then EOF to the client after the
/// server side closes.
#[cfg(any(feature = "sim", unix))]
fn armed_tcp_round<IO: Io>(io: &mut IO, out: &mut Completions, tcp_addr: SocketAddr) {
    let bound = io.listen(Sock(0), tcp_addr).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, out, 50, |v| v.len() == 2).unwrap();
    let srv_sock =
        Sock(u32::try_from(cs.iter().find(|c| c.token == Token(1)).unwrap().result).unwrap());
    io.submit(Token(3), Op::ArmedRecv { sock: srv_sock })
        .unwrap();

    write_pool_bytes(io, BufId(0), 0, b"shared-armed-tcp");
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 16,
            zero_copy: false,
        },
    )
    .unwrap();
    let received = drive_until(io, out, 50, |v| v.iter().any(|c| c.token == Token(3))).unwrap();
    let rc = received.iter().find(|c| c.token == Token(3)).unwrap();
    let id = rc.buf.unwrap();
    assert_eq!(
        read_pool_bytes(io, id, 0, usize::try_from(rc.result).unwrap()),
        b"shared-armed-tcp"
    );
    io.give_back(id);

    io.submit(Token(5), Op::Close { sock: srv_sock }).unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(io, out, 50, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more) && v.iter().any(|c| c.token == Token(5))
    })
    .unwrap();
    io.submit(
        Token(6),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let eof = drive_until(io, out, 50, |v| v.iter().any(|c| c.token == Token(6))).unwrap();
    assert_eq!(eof[0].result, 0);
    io.submit(Token(7), Op::Close { sock: Sock(1) }).unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(io, out, 30, |v| v.iter().any(|c| c.token == Token(7))).unwrap();
}

/// A Unix-domain accept, connect and exchange on a socket in a fresh `tag` dir.
#[cfg(any(feature = "sim", unix))]
fn unix_round<IO: Io>(io: &mut IO, out: &mut Completions, tag: &str) {
    let dir = TempDir::new(tag).unwrap();
    let upath = dir.socket_path("suite.sock");
    io.listen_unix(Sock(2), upath.to_str().unwrap()).unwrap();
    let plen = write_nul_path(io, BufId(0), 0, upath.to_str().unwrap());
    io.submit(
        Token(8),
        Op::Accept {
            listener: Sock(2),
            sock: Sock(4),
        },
    )
    .unwrap();
    io.submit(
        Token(9),
        Op::ConnectUnix {
            sock: Sock(3),
            path_buf: BufId(0),
            path_off: 0,
            path_len: plen,
        },
    )
    .unwrap();
    let unix_connected = drive_until(io, out, 50, |v| v.len() == 2).unwrap();
    for c in unix_connected {
        assert_eq!(c.result, 0);
    }
    write_pool_bytes(io, BufId(1), 0, b"unix-shared");
    io.submit(
        Token(10),
        Op::Send {
            sock: Sock(3),
            buf: BufId(1),
            off: 0,
            len: 11,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(
        Token(11),
        Op::Recv {
            sock: Sock(4),
            buf: BufId(2),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let exchanged = drive_until(io, out, 50, |v| v.len() == 2).unwrap();
    assert_eq!(
        exchanged
            .iter()
            .find(|c| c.token == Token(11))
            .unwrap()
            .result,
        11
    );
    assert_eq!(read_pool_bytes(io, BufId(2), 0, 11), b"unix-shared");
}

#[cfg(any(feature = "sim", unix))]
fn run_server_net_suite<IO: Io>(io: &mut IO, tcp_addr: SocketAddr, tag: &str) {
    let mut out = Completions::new(32);
    armed_tcp_round(io, &mut out, tcp_addr);
    // `#44` § Every backend specifies identical Unix-domain socket and armed accept/recv
    // behavior across `SimIo`, `Posix`, and `UringIo`.
    unix_round(io, &mut out, tag);
}

#[cfg(any(feature = "sim", unix))]
#[test]
fn t1_f23_1_shared_server_net_suite_passes_across_backends() {
    #[cfg(feature = "sim")]
    {
        let mut sim = SimIo::new(server_limits(), 2301, Faults::default());
        run_server_net_suite(&mut sim, "192.0.2.23:9300".parse().unwrap(), "sim_net");
    }
    #[cfg(unix)]
    {
        let mut posix = posix_backend(server_limits()).expect("posix");
        run_server_net_suite(&mut posix, "127.0.0.1:0".parse().unwrap(), "posix_net");
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(mut uring) = UringIo::new(server_limits()) {
            run_server_net_suite(&mut uring, "127.0.0.1:0".parse().unwrap(), "uring_net");
        }
    }
}

/// Accepts `Sock(2)` (`Token(1)`) and connects `Sock(1)` (`Token(2)`) to
/// `addr`, driving up to `ticks` ticks for both.
#[cfg(feature = "sim")]
pub(super) fn sim_accept_connect(
    io: &mut SimIo,
    out: &mut Completions,
    addr: SocketAddr,
    ticks: usize,
) {
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(io, out, ticks, |v| v.len() == 2).unwrap();
}

/// Submits a `Send` of `BufId(0)[off..off + len]` on `Sock(1)` as `Token(4)`.
#[cfg(feature = "sim")]
fn send_from(io: &mut SimIo, off: usize, len: usize) {
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: off as u32,
            len: len as u32,
            zero_copy: false,
        },
    )
    .unwrap();
}

/// Sends `expected` from `Sock(1)` (resending after short sends) while
/// `Sock(2)`'s armed recv (`Token(3)`) collects it; returns what arrived.
#[cfg(feature = "sim")]
fn stream_into_armed_recv(io: &mut SimIo, out: &mut Completions, expected: &[u8]) -> Vec<u8> {
    write_pool_bytes(io, BufId(0), 0, expected);
    let mut sent = 0usize;
    let mut recvd = Vec::new();
    send_from(io, 0, expected.len());

    for _ in 0..400 {
        if recvd.len() >= expected.len() {
            break;
        }
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            match c.token.0 {
                4 => {
                    assert!(c.result > 0);
                    sent += usize::try_from(c.result).unwrap();
                    if sent < expected.len() {
                        send_from(io, sent, expected.len() - sent);
                    }
                }
                3 => {
                    assert!(c.result > 0);
                    let id = c.buf.unwrap();
                    recvd.extend_from_slice(&read_pool_bytes(
                        io,
                        id,
                        0,
                        usize::try_from(c.result).unwrap(),
                    ));
                    io.give_back(id);
                }
                _ => {}
            }
        }
    }
    recvd
}

#[cfg(feature = "sim")]
#[test]
fn t1_f23_2_sim_armed_recv_delivers_exact_stream_under_partial_short_delay_faults() {
    let faults = Faults {
        partial_recv: 300_000,
        short_send: 300_000,
        delay: 200_000,
        ..Default::default()
    };
    let mut io = SimIo::new(server_limits(), 2302, faults);
    let mut out = Completions::new(32);
    let addr = io
        .listen(Sock(0), "192.0.2.23:9302".parse().unwrap())
        .unwrap();
    sim_accept_connect(&mut io, &mut out, addr, 100);

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let expected: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
    let recvd = stream_into_armed_recv(&mut io, &mut out, &expected);
    assert_eq!(recvd, expected);
}
