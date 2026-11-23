// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 22 Tier 1 E2E tests: shutdown-before-close, socket options & Unix sockets.

#[cfg(any(feature = "sim", unix))]
use super::super::harness::drive_until;
#[cfg(unix)]
use super::super::harness::{
    posix_backend, read_pool_bytes, write_nul_path, write_pool_bytes, TempDir,
};
#[cfg(any(feature = "sim", unix))]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(any(feature = "sim", unix))]
use std::net::SocketAddr;
#[cfg(feature = "sim")]
use std::net::{IpAddr, Ipv4Addr};

#[cfg(feature = "sim")]
pub(super) const SIM_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 22)), 9200);

#[cfg(any(feature = "sim", unix))]
pub(super) fn unix_limits() -> Limits {
    Limits {
        sockets_max: 16,
        buffers: 4,
        buffer_size: 2048,
        ops_max: 16,
        accept_range_start: 8,
        accept_range_len: 4,
        ring_buffers: 8,
        ring_buffer_size: 1024,
        ..Limits::BASE
    }
}

#[cfg(any(feature = "sim", unix))]
fn check_shutdown_before_close<IO: Io>(io: &mut IO, addr: SocketAddr) {
    let mut out = Completions::new(16);
    let bound = io.listen(Sock(0), addr).unwrap();
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
            addr: bound,
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(io, &mut out, 50, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    io.tick(false, &mut out).unwrap();
    io.submit(Token(4), Op::Close { sock: Sock(2) }).unwrap();
    let cs = drive_until(io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more) && v.iter().any(|c| c.token == Token(4))
    })
    .unwrap();
    assert!(!cs.iter().find(|c| c.token == Token(3)).unwrap().more);

    io.submit(
        Token(5),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let cs = drive_until(io, &mut out, 50, |v| v.iter().any(|c| c.token == Token(5))).unwrap();
    assert_eq!(
        cs[0].result, 0,
        "peer must see EOF after shutdown-before-close"
    );
}

#[cfg(any(feature = "sim", unix))]
#[test]
fn t1_f22_1_shutdown_before_close_signals_eof_on_sim_and_posix() {
    #[cfg(feature = "sim")]
    {
        let mut sim = SimIo::new(unix_limits(), 2201, Faults::default());
        check_shutdown_before_close(&mut sim, SIM_ADDR);
    }

    #[cfg(unix)]
    {
        let mut posix = posix_backend(unix_limits()).expect("posix");
        check_shutdown_before_close(&mut posix, "127.0.0.1:0".parse().unwrap());
    }
}

#[cfg(unix)]
#[test]
fn t1_f22_2_socket_options_nodelay_and_keepalive_configure_cleanly() {
    let mut posix = posix_backend(unix_limits()).expect("posix");
    posix.no_delay(true);
    posix.keep_alive(true);
    let mut out = Completions::new(16);
    let bound = posix
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    posix
        .submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
    posix
        .submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr: bound,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 2).unwrap();
    for c in cs {
        assert_eq!(c.result, 0, "connect/accept with TCP opts failed: {c:?}");
    }
    write_pool_bytes(&mut posix, BufId(0), 0, b"nodelay");
    posix
        .submit(
            Token(3),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 7,
                zero_copy: false,
            },
        )
        .unwrap();
    posix
        .submit(
            Token(4),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(1),
                off: 0,
                len: 64,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 2).unwrap();
    assert!(cs.iter().any(|c| c.token == Token(4) && c.result == 7));
}

#[cfg(unix)]
#[test]
fn t1_f22_3_unix_domain_socket_listen_connect_and_echo() {
    let dir = TempDir::new("unix_echo").expect("tempdir");
    let sock_path = dir.socket_path("echo.sock");
    let mut posix = posix_backend(unix_limits()).expect("posix");
    let mut out = Completions::new(16);

    posix
        .listen_unix(Sock(0), sock_path.to_str().unwrap())
        .expect("listen_unix");
    let path_len = write_nul_path(&mut posix, BufId(0), 0, sock_path.to_str().unwrap());
    posix
        .submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
    posix
        .submit(
            Token(2),
            Op::ConnectUnix {
                sock: Sock(1),
                path_buf: BufId(0),
                path_off: 0,
                path_len,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 2).unwrap();
    for c in cs {
        assert_eq!(c.result, 0);
    }

    write_pool_bytes(&mut posix, BufId(1), 0, b"unix-stream-payload");
    posix
        .submit(
            Token(3),
            Op::Send {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 19,
                zero_copy: false,
            },
        )
        .unwrap();
    posix
        .submit(
            Token(4),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(2),
                off: 0,
                len: 64,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 2).unwrap();
    assert_eq!(cs.iter().find(|c| c.token == Token(4)).unwrap().result, 19);
    assert_eq!(
        read_pool_bytes(&mut posix, BufId(2), 0, 19),
        b"unix-stream-payload"
    );
}

#[cfg(unix)]
#[test]
fn t1_f22_4_unix_domain_armed_accept_and_recv_with_give_back() {
    let dir = TempDir::new("unix_armed").expect("tempdir");
    let sock_path = dir.socket_path("armed.sock");
    let mut posix = posix_backend(unix_limits()).expect("posix");
    let mut out = Completions::new(16);

    posix
        .listen_unix(Sock(0), sock_path.to_str().unwrap())
        .expect("listen_unix");
    posix
        .submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    let path_len = write_nul_path(&mut posix, BufId(0), 0, sock_path.to_str().unwrap());
    posix
        .submit(
            Token(2),
            Op::ConnectUnix {
                sock: Sock(1),
                path_buf: BufId(0),
                path_off: 0,
                path_len,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 2).unwrap();
    let acc = cs.iter().find(|c| c.token == Token(1)).unwrap();
    assert!(acc.more);
    let srv_sock = Sock(acc.result as u32);
    assert!((8..12).contains(&srv_sock.0));

    posix
        .submit(Token(3), Op::ArmedRecv { sock: srv_sock })
        .unwrap();
    write_pool_bytes(&mut posix, BufId(1), 0, b"armed-unix");
    posix
        .submit(
            Token(4),
            Op::Send {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 10,
                zero_copy: false,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(3))
    })
    .unwrap();
    let rc = cs.iter().find(|c| c.token == Token(3)).unwrap();
    assert!(rc.more);
    let id = rc.buf.expect("ring buf");
    assert_eq!(read_pool_bytes(&mut posix, id, 0, 10), b"armed-unix");
    posix.give_back(id);
}

#[cfg(unix)]
#[test]
fn t1_f22_5_mixed_tcp_and_unix_listeners_skip_tcp_opts_on_unix() {
    let dir = TempDir::new("unix_mixed").expect("tempdir");
    let sock_path = dir.socket_path("mixed.sock");
    let mut posix = posix_backend(unix_limits()).expect("posix");
    posix.no_delay(true);
    posix.keep_alive(true);
    let mut out = Completions::new(16);

    let tcp_addr = posix
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    posix
        .listen_unix(Sock(3), sock_path.to_str().unwrap())
        .unwrap();

    posix
        .submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
    posix
        .submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr: tcp_addr,
            },
        )
        .unwrap();

    let path_len = write_nul_path(&mut posix, BufId(0), 0, sock_path.to_str().unwrap());
    posix
        .submit(
            Token(3),
            Op::Accept {
                listener: Sock(3),
                sock: Sock(5),
            },
        )
        .unwrap();
    posix
        .submit(
            Token(4),
            Op::ConnectUnix {
                sock: Sock(4),
                path_buf: BufId(0),
                path_off: 0,
                path_len,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 4).unwrap();
    for c in cs {
        assert_eq!(c.result, 0, "mixed TCP/Unix failed: {c:?}");
    }
}
