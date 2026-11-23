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

#[cfg(any(feature = "sim", unix))]
fn run_server_net_suite<IO: Io>(io: &mut IO, tcp_addr: SocketAddr, tag: &str) {
    let mut out = Completions::new(32);
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
    let cs = drive_until(io, &mut out, 50, |v| v.len() == 2).unwrap();
    let srv_sock = Sock(cs.iter().find(|c| c.token == Token(1)).unwrap().result as u32);
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
    let cs = drive_until(io, &mut out, 50, |v| v.iter().any(|c| c.token == Token(3))).unwrap();
    let rc = cs.iter().find(|c| c.token == Token(3)).unwrap();
    let id = rc.buf.unwrap();
    assert_eq!(
        read_pool_bytes(io, id, 0, rc.result as usize),
        b"shared-armed-tcp"
    );
    io.give_back(id);

    io.submit(Token(5), Op::Close { sock: srv_sock }).unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(io, &mut out, 50, |v| {
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
    let cs = drive_until(io, &mut out, 50, |v| v.iter().any(|c| c.token == Token(6))).unwrap();
    assert_eq!(cs[0].result, 0);
    io.submit(Token(7), Op::Close { sock: Sock(1) }).unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(io, &mut out, 30, |v| v.iter().any(|c| c.token == Token(7))).unwrap();

    // `#44` § Every backend specifies identical Unix-domain socket and armed accept/recv
    // behavior across `SimIo`, `Posix`, and `UringIo`.
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
    let cs = drive_until(io, &mut out, 50, |v| v.len() == 2).unwrap();
    for c in cs {
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
    let cs = drive_until(io, &mut out, 50, |v| v.len() == 2).unwrap();
    assert_eq!(cs.iter().find(|c| c.token == Token(11)).unwrap().result, 11);
    assert_eq!(read_pool_bytes(io, BufId(2), 0, 11), b"unix-shared");
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
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 100, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let expected: Vec<u8> = (0..1500).map(|i| (i % 251) as u8).collect();
    write_pool_bytes(&mut io, BufId(0), 0, &expected);
    let mut sent = 0usize;
    let mut recvd = Vec::new();
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: expected.len() as u32,
            zero_copy: false,
        },
    )
    .unwrap();

    for _ in 0..400 {
        if recvd.len() >= expected.len() {
            break;
        }
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            match c.token.0 {
                4 => {
                    assert!(c.result > 0);
                    sent += c.result as usize;
                    if sent < expected.len() {
                        io.submit(
                            Token(4),
                            Op::Send {
                                sock: Sock(1),
                                buf: BufId(0),
                                off: sent as u32,
                                len: (expected.len() - sent) as u32,
                                zero_copy: false,
                            },
                        )
                        .unwrap();
                    }
                }
                3 => {
                    assert!(c.result > 0);
                    let id = c.buf.unwrap();
                    recvd.extend_from_slice(&read_pool_bytes(&mut io, id, 0, c.result as usize));
                    io.give_back(id);
                }
                _ => {}
            }
        }
    }
    assert_eq!(recvd, expected);
}
