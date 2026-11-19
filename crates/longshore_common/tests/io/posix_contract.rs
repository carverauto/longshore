// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared contract, `ReadFile`, errno mapping, and UDP tests for `PosixIo`.

#![cfg(unix)]

use longshore_common::io::posix::errno::{linux, to_linux_errno};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{
    contract, contract_net, BufId, Completions, Io, Limits, Op, Sock, Token,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{SystemTime, UNIX_EPOCH};

const LIMITS: Limits = Limits {
    sockets_max: 12,
    buffers: 4,
    buffer_size: 65536,
    ops_max: 16,
    ..Limits::BASE
};

fn v4() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn v6() -> SocketAddr {
    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0)
}

fn drain_n(io: &mut PosixIo, out: &mut Completions, want: usize) -> Vec<(u64, i32)> {
    let mut got = Vec::with_capacity(want);
    while got.len() < want {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            got.push((c.token.0, c.result));
        }
    }
    got
}

#[test]
fn posix_keeps_the_shared_io_contract_v4_and_v6() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p4 = std::env::temp_dir().join(format!("longshore-posix-v4-{unique}.sock"));
    let p6 = std::env::temp_dir().join(format!("longshore-posix-v6-{unique}.sock"));
    let _ = std::fs::remove_file(&p4);
    let _ = std::fs::remove_file(&p6);
    contract::run_all(&mut |l| PosixIo::new(l).unwrap(), v4());
    contract::run_all(&mut |l| PosixIo::new(l).unwrap(), v6());
    contract_net::run_all(
        &mut |l| PosixIo::new(l).unwrap(),
        v4(),
        p4.to_str().unwrap(),
    );
    contract_net::run_all(
        &mut |l| PosixIo::new(l).unwrap(),
        v6(),
        p6.to_str().unwrap(),
    );
    let _ = std::fs::remove_file(&p4);
    let _ = std::fs::remove_file(&p6);
}

#[test]
fn read_file_and_errno_translation() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("longshore-posix-{unique}.txt"));
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(4);
    let f = io.register_file(path.to_str().unwrap()).unwrap();

    io.submit(
        Token(1),
        Op::ReadFile {
            file: f,
            buf: BufId(0),
            off: 0,
            len: 32,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(1, -linux::ENOENT)]);

    std::fs::write(&path, b"posix-file-body").unwrap();
    io.submit(
        Token(2),
        Op::ReadFile {
            file: f,
            buf: BufId(0),
            off: 0,
            len: 32,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(2, 15)]);
    assert_eq!(&io.buf(BufId(0))[..15], b"posix-file-body");
    let _ = std::fs::remove_file(&path);

    // Unknown errno falls back to -EIO.
    assert_eq!(to_linux_errno(99_999), -linux::EIO);
    #[cfg(target_os = "macos")]
    {
        assert_eq!(to_linux_errno(35), -linux::EAGAIN);
        assert_eq!(to_linux_errno(54), -linux::ECONNRESET);
        assert_eq!(to_linux_errno(61), -linux::ECONNREFUSED);
        assert_eq!(to_linux_errno(89), -linux::ECANCELED);
        assert_eq!(to_linux_errno(11), -linux::EDEADLK);
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(to_linux_errno(11), -linux::EAGAIN);
        assert_eq!(to_linux_errno(104), -linux::ECONNRESET);
        assert_eq!(to_linux_errno(111), -linux::ECONNREFUSED);
        assert_eq!(to_linux_errno(125), -linux::ECANCELED);
    }
}

#[test]
fn udp_connect_send_and_recv() {
    let srv = UdpSocket::bind(v4()).unwrap();
    let srv_addr = srv.local_addr().unwrap();
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(4);

    io.submit(
        Token(1),
        Op::ConnectUdp {
            sock: Sock(0),
            addr: srv_addr,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(1, 0)]);

    io.buf(BufId(0))[..4].copy_from_slice(b"ping");
    io.submit(
        Token(2),
        Op::Send {
            sock: Sock(0),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(2, 4)]);

    let mut rx = [0u8; 16];
    let (n, from) = srv.recv_from(&mut rx).unwrap();
    assert_eq!(&rx[..n], b"ping");
    srv.send_to(b"pong", from).unwrap();

    io.submit(
        Token(3),
        Op::Recv {
            sock: Sock(0),
            buf: BufId(1),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    assert_eq!(drain_n(&mut io, &mut out, 1), vec![(3, 4)]);
    assert_eq!(&io.buf(BufId(1))[..4], b"pong");
}
