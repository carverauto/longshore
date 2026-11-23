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

/// Submits `op` as `tok`, ticks once, and returns the completion's result.
fn run_one(io: &mut PosixIo, out: &mut Completions, tok: u64, op: Op) -> i32 {
    io.submit(Token::new(tok), op).unwrap();
    io.tick(true, out).unwrap();
    out.pop().unwrap().result()
}

/// Writes `path` and a NUL into buffer 0; the length written.
fn put_path(io: &mut PosixIo, path: &str) -> u32 {
    let mut p = path.as_bytes().to_vec();
    p.push(0);
    io.buf(BufId::new(0))[..p.len()].copy_from_slice(&p);
    p.len() as u32
}

/// Opens the path in buffer 0, `path_len` bytes, into `slot`.
fn open(slot: u32, path_len: u32, flags: OpenFlags) -> Op {
    Op::FileOpen {
        slot: FileSlot::new(slot),
        path_buf: BufId::new(0),
        path_off: 0,
        path_len,
        flags,
    }
}

/// Ops on slot 0 that are invalid for their arguments: zero-length reserve
/// and punch, and reads and writes at `u64::MAX`.
fn invalid_file_ops() -> [(u64, Op); 4] {
    [
        (
            2,
            Op::FileReserve {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 0,
                keep_size: false,
            },
        ),
        (
            3,
            Op::FilePunch {
                slot: FileSlot::new(0),
                file_off: 0,
                len: 0,
            },
        ),
        (
            4,
            Op::FileRead {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 4,
                file_off: u64::MAX,
            },
        ),
        (
            5,
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 4,
                file_off: u64::MAX,
            },
        ),
    ]
}

/// A 104-byte Unix socket path, one past `sun_path` on macOS: listening on it
/// and connecting to it fail there.
fn sun_path_one_too_long(io: &mut PosixIo, out: &mut Completions) {
    let mut long_path = [b'x'; 105];
    long_path[104] = 0;
    io.buf(BufId::new(0))[..105].copy_from_slice(&long_path);
    let long_str = core::str::from_utf8(&long_path[..104]).unwrap();
    if cfg!(target_os = "macos") {
        assert!(io.listen_unix(Sock::new(1), long_str).is_err());
    }
    let connect = Op::ConnectUnix {
        sock: Sock::new(1),
        path_buf: BufId::new(0),
        path_off: 0,
        path_len: 105,
    };
    let result = run_one(io, out, 8, connect);
    if cfg!(target_os = "macos") {
        assert_eq!(result, -ENAMETOOLONG);
    }
}

#[test]
fn posix_file_edge_cases_and_sun_path_boundary() {
    let dir = std::env::temp_dir().join(format!("longshore_posix_files_{}", std::process::id()));
    // A file left by an earlier run would get in the way; usually there is none.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    let mut io = PosixIo::new(
        Limits::BASE
            .with_sockets_max(2)
            .with_buffers(2)
            .with_buffer_size(512)
            .with_files_max(2)
            .with_ops_max(4),
    )
    .unwrap();
    let mut out = Completions::new(4);

    let fpath = dir.join("edge.dat");
    let file_len = put_path(&mut io, fpath.to_str().unwrap());
    let read_write = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        run_one(&mut io, &mut out, 1, open(0, file_len, read_write)),
        0
    );

    for (tok, op) in invalid_file_ops() {
        assert_eq!(run_one(&mut io, &mut out, tok, op), -EINVAL);
    }

    let dir_len = put_path(&mut io, dir.to_str().unwrap());
    let directory = OpenFlags::NONE
        .with_read(true)
        .with_direct(true)
        .with_directory(true);
    let d_res = run_one(&mut io, &mut out, 6, open(1, dir_len, directory));
    if d_res == 0 {
        let close = Op::FileClose {
            slot: FileSlot::new(1),
        };
        assert_eq!(run_one(&mut io, &mut out, 7, close), 0);
    }

    sun_path_one_too_long(&mut io, &mut out);
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
}

/// Up to 32 bytes from Sock(0) into buffer 0, if any are there now.
const RECV_NOW: Op = Op::RecvNow {
    sock: Sock::new(0),
    buf: BufId::new(0),
    off: 0,
    len: 32,
};

/// Connects Sock(0) to `udp_srv`: `RecvNow` finds nothing, then "ping" goes
/// out and the server's "pong" is there for the next `RecvNow`.
fn udp_recv_now_round_trip(io: &mut PosixIo, out: &mut Completions, udp_srv: &UdpSocket) {
    let addr = udp_srv.local_addr().unwrap();
    let connect = Op::ConnectUdp {
        sock: Sock::new(0),
        addr,
    };
    assert_eq!(run_one(io, out, 1, connect), 0);
    assert_eq!(run_one(io, out, 2, RECV_NOW), -EAGAIN);

    io.buf(BufId::new(0))[..4].copy_from_slice(b"ping");
    let send = Op::Send {
        sock: Sock::new(0),
        buf: BufId::new(0),
        off: 0,
        len: 4,
        zero_copy: false,
    };
    assert_eq!(run_one(io, out, 3, send), 4);

    let mut tmp = [0u8; 16];
    let (n, peer) = udp_srv.recv_from(&mut tmp).unwrap();
    assert_eq!(&tmp[..n], b"ping");
    let _: usize = udp_srv.send_to(b"pong", peer).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));

    assert_eq!(run_one(io, out, 4, RECV_NOW), 4);
    assert_eq!(&io.buf(BufId::new(0))[..4], b"pong");
}

/// Listens on Sock(1), accepts into Sock(2) the connect of Sock(3).
fn tcp_pair(io: &mut PosixIo, out: &mut Completions) {
    let tcp_addr = io
        .listen(Sock::new(1), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    io.submit(
        Token::new(5),
        Op::Accept {
            listener: Sock::new(1),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(6),
        Op::Connect {
            sock: Sock::new(3),
            addr: tcp_addr,
        },
    )
    .unwrap();
    let mut up = 0;
    while up < 2 {
        io.tick(true, out).unwrap();
        while let Some(conn) = out.pop() {
            assert_eq!(conn.result(), 0);
            up += 1;
        }
    }
}

/// Zero-copy sends from Sock(3) until one stalls; its token.
fn zero_copy_until_stalled(io: &mut PosixIo, out: &mut Completions, tok: &mut u64) -> u64 {
    io.buf(BufId::new(1)).fill(0x5A);
    let mut pending_zc = None;
    for _ in 0..256 {
        let send = Op::Send {
            sock: Sock::new(3),
            buf: BufId::new(1),
            off: 0,
            len: 65536,
            zero_copy: true,
        };
        io.submit(Token::new(*tok), send).unwrap();
        io.tick(false, out).unwrap();
        if let Some(sent) = out.pop() {
            assert!(sent.result() > 0);
            *tok += 1;
        } else {
            pending_zc = Some(*tok);
            *tok += 1;
            break;
        }
    }
    pending_zc.expect("send buffer should stall")
}

/// Reads from Sock(2) until the stalled zero-copy send `zc_tok` completes.
fn drain_until_sent(io: &mut PosixIo, out: &mut Completions, tok: &mut u64, zc_tok: u64) {
    let mut done = false;
    while !done {
        let recv = Op::Recv {
            sock: Sock::new(2),
            buf: BufId::new(2),
            off: 0,
            len: 65536,
        };
        io.submit(Token::new(*tok), recv).unwrap();
        *tok += 1;
        io.tick(true, out).unwrap();
        while let Some(drained) = out.pop() {
            if drained.token().get() == zc_tok {
                assert!(drained.result() > 0);
                done = true;
            }
        }
    }
}

#[test]
fn posix_udp_recv_now_and_send_zero_copy_backpressure() {
    let udp_srv = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();

    let mut io = PosixIo::new(
        Limits::BASE
            .with_sockets_max(4)
            .with_buffers(3)
            .with_buffer_size(65536)
            .with_ops_max(8),
    )
    .unwrap();
    let mut out = Completions::new(8);

    udp_recv_now_round_trip(&mut io, &mut out, &udp_srv);
    tcp_pair(&mut io, &mut out);

    let mut tok = 100u64;
    let zc_tok = zero_copy_until_stalled(&mut io, &mut out, &mut tok);
    drain_until_sent(&mut io, &mut out, &mut tok, zc_tok);
}
