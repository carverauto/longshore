// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial `PosixIo` file-op boundaries, `sun_path` limits,
//! UDP `RecvNow`, and `Send { zero_copy: true }` backpressure.

use longshore_common::io::posix::PosixIo;
use longshore_common::io::{
    BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token, EAGAIN, EINVAL,
    ENAMETOOLONG,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

#[test]
fn posix_file_edge_cases_and_sun_path_boundary() {
    let dir = std::env::temp_dir().join(format!("longshore_posix_files_{}", std::process::id()));
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    let mut io = PosixIo::new(Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 512,
        files_max: 2,
        ops_max: 4,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(4);

    let fpath = dir.join("edge.dat");
    let mut p = fpath.to_str().unwrap().as_bytes().to_vec();
    p.push(0);
    io.buf(BufId(0))[..p.len()].copy_from_slice(&p);
    io.submit(
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: p.len() as u32,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);

    for (tok, op) in [
        (
            2,
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 0,
                keep_size: false,
            },
        ),
        (
            3,
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 0,
                len: 0,
            },
        ),
        (
            4,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4,
                file_off: u64::MAX,
            },
        ),
        (
            5,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4,
                file_off: u64::MAX,
            },
        ),
    ] {
        io.submit(Token(tok), op).unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, -EINVAL);
    }

    let mut dp = dir.to_str().unwrap().as_bytes().to_vec();
    dp.push(0);
    io.buf(BufId(0))[..dp.len()].copy_from_slice(&dp);
    io.submit(
        Token(6),
        Op::FileOpen {
            slot: FileSlot(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: dp.len() as u32,
            flags: OpenFlags {
                read: true,
                direct: true,
                directory: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    let d_res = out.pop().unwrap().result;
    if d_res == 0 {
        io.submit(Token(7), Op::FileClose { slot: FileSlot(1) })
            .unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, 0);
    }

    let mut long_path = [b'x'; 105];
    long_path[104] = 0;
    io.buf(BufId(0))[..105].copy_from_slice(&long_path);
    let long_str = core::str::from_utf8(&long_path[..104]).unwrap();
    if cfg!(target_os = "macos") {
        assert!(io.listen_unix(Sock(1), long_str).is_err());
    }
    io.submit(
        Token(8),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: 105,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    let c = out.pop().unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(c.result, -ENAMETOOLONG);
    }
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn posix_udp_recv_now_and_send_zero_copy_backpressure() {
    let udp_srv = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let udp_addr = udp_srv.local_addr().unwrap();

    let mut io = PosixIo::new(Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 65536,
        ops_max: 8,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(8);

    io.submit(
        Token(1),
        Op::ConnectUdp {
            sock: Sock(0),
            addr: udp_addr,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);

    io.submit(
        Token(2),
        Op::RecvNow {
            sock: Sock(0),
            buf: BufId(0),
            off: 0,
            len: 32,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, -EAGAIN);

    io.buf(BufId(0))[..4].copy_from_slice(b"ping");
    io.submit(
        Token(3),
        Op::Send {
            sock: Sock(0),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 4);

    let mut tmp = [0u8; 16];
    let (n, peer) = udp_srv.recv_from(&mut tmp).unwrap();
    assert_eq!(&tmp[..n], b"ping");
    let _: usize = udp_srv.send_to(b"pong", peer).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));

    io.submit(
        Token(4),
        Op::RecvNow {
            sock: Sock(0),
            buf: BufId(0),
            off: 0,
            len: 32,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    let c = out.pop().unwrap();
    assert_eq!(c.result, 4);
    assert_eq!(&io.buf(BufId(0))[..4], b"pong");

    let tcp_addr = io.listen(Sock(1), "127.0.0.1:0".parse().unwrap()).unwrap();
    io.submit(
        Token(5),
        Op::Accept {
            listener: Sock(1),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(6),
        Op::Connect {
            sock: Sock(3),
            addr: tcp_addr,
        },
    )
    .unwrap();
    let mut up = 0;
    while up < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(conn) = out.pop() {
            assert_eq!(conn.result, 0);
            up += 1;
        }
    }

    io.buf(BufId(1)).fill(0x5A);
    let mut tok = 100u64;
    let mut pending_zc = None;
    for _ in 0..256 {
        io.submit(
            Token(tok),
            Op::Send {
                sock: Sock(3),
                buf: BufId(1),
                off: 0,
                len: 65536,
                zero_copy: true,
            },
        )
        .unwrap();
        io.tick(false, &mut out).unwrap();
        if let Some(sent) = out.pop() {
            assert!(sent.result > 0);
            tok += 1;
        } else {
            pending_zc = Some(tok);
            tok += 1;
            break;
        }
    }
    let zc_tok = pending_zc.expect("send buffer should stall");
    let mut done = false;
    while !done {
        io.submit(
            Token(tok),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(2),
                off: 0,
                len: 65536,
            },
        )
        .unwrap();
        tok += 1;
        io.tick(true, &mut out).unwrap();
        while let Some(drained) = out.pop() {
            if drained.token.0 == zc_tok {
                assert!(drained.result > 0);
                done = true;
            }
        }
    }
}
