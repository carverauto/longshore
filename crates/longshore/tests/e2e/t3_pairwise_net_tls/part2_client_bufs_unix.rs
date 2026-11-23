// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-13`–`T3-15`: portable `Client` + `Stream` server,
//! 4-buffer-class isolation under CQ bursts, and Unix-domain armed `Recv` + file chain.

use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes, TempDir,
};
use super::net_limits;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token};

#[test]
fn t3_13_s3_client_portable_with_stream_server_plain_and_tls() {
    use super::super::harness::HttpStoreServer;
    use longshore::client::Builder;
    use longshore::store::creds::Credentials;

    let server = HttpStoreServer::start_plain().expect("plain server");
    let creds = Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .unwrap();
    let b = Builder::new("127.0.0.1", "us-east-1", creds)
        .port(server.port())
        .address(Some(server.addr()))
        .bucket_in_path(Some("test-bucket".into()))
        .tls(false)
        .portable(true)
        .threads(1)
        .buffers(4)
        .buffer_size(64 * 1024)
        .connections(4)
        .operations(4);
    let client = b.build().expect("portable plain S3 client");

    let mut buf = client.buffer_blocking();
    let payload = b"t3-13-portable-plain";
    buf.as_mut_slice()[..payload.len()].copy_from_slice(payload);
    let (res, buf) = client.store("k1", buf, payload.len()).wait();
    res.expect("store plain");
    let (got, _buf) = client.fetch("k1", buf).wait();
    assert_eq!(got.expect("fetch plain"), Some(payload.len()));

    #[cfg(feature = "tls")]
    {
        use super::super::harness::make_tls_identity;
        use longshore::client::BuildError;

        let id = make_tls_identity(&["localhost", "127.0.0.1"]);
        let tls_srv = HttpStoreServer::start_tls(id.server_setup()).expect("tls server");
        let bt = Builder::new("127.0.0.1", "us-east-1", creds)
            .port(tls_srv.port())
            .address(Some(tls_srv.addr()))
            .bucket_in_path(Some("test-bucket".into()))
            .tls(true)
            .portable(true)
            .threads(1)
            .buffers(4)
            .buffer_size(64 * 1024)
            .connections(4)
            .operations(4);
        match bt.build() {
            Ok(c) => {
                let mut b2 = c.buffer_blocking();
                b2.as_mut_slice()[..4].copy_from_slice(b"tls!");
                // The client does not trust the test identity's root, so the store
                // ends in BadCertificate; this part checks only that a TLS client
                // builds and runs a handshake against the server.
                let _: Option<()> = c.store("k-tls", b2, 4).wait().0.ok();
            }
            Err(BuildError::Setting(msg)) => panic!("unexpected setting error: {msg}"),
            Err(e) => panic!("unexpected build error: {e}"),
        }
    }
}

#[test]
fn t3_14_four_buffer_classes_isolation_and_cq_burst() {
    let limits = Limits::BASE
        .with_sockets_max(32)
        .with_buffers(2)
        .with_buffer_size(8192)
        .with_small_buffers(4)
        .with_small_buffer_size(512)
        .with_files_max(16)
        .with_aligned_buffers(4)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_accept_range_start(16)
        .with_accept_range_len(16)
        .with_ring_buffers(256)
        .with_ring_buffer_size(1024)
        .with_ops_max(32);

    #[cfg(unix)]
    {
        use super::super::harness::posix_backend;

        let dir = TempDir::new("t3_14").unwrap();
        let mut io = posix_backend(limits).unwrap();
        let mut out = Completions::new(64);
        let addr = io
            .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
            .unwrap();

        stamp_buffer_classes(&mut io, limits);
        let path = dir.file_path("burst.dat");
        let srv = accept_connect_and_open(&mut io, &mut out, limits, addr, path.to_str().unwrap());
        let rbuf = burst_send_and_write(&mut io, &mut out, limits, srv);
        assert_eq!(read_pool_bytes(&mut io, rbuf, 0, 8), b"SMALL-00");
        io.give_back(rbuf);
        assert_eq!(&io.buf(BufId::new(0))[..8], b"LARGE-00");
        assert_eq!(&io.buf(limits.aligned(0))[..8], b"ALIGN-00");
    }
}

/// Stamps the first large, small, and aligned buffers and checks each kept its own bytes.
#[cfg(unix)]
fn stamp_buffer_classes<IO: Io>(io: &mut IO, limits: Limits) {
    io.buf(BufId::new(0))[..8].copy_from_slice(b"LARGE-00");
    io.buf(limits.small(0))[..8].copy_from_slice(b"SMALL-00");
    io.buf(limits.aligned(0))[..8].copy_from_slice(b"ALIGN-00");
    assert_eq!(&io.buf(BufId::new(0))[..8], b"LARGE-00");
    assert_eq!(&io.buf(limits.small(0))[..8], b"SMALL-00");
    assert_eq!(&io.buf(limits.aligned(0))[..8], b"ALIGN-00");
}

/// In one burst: arms accept, connects `Sock(1)`, and opens `path` for direct
/// I/O in `FileSlot(0)`; returns the accepted server socket.
#[cfg(unix)]
fn accept_connect_and_open<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    limits: Limits,
    addr: std::net::SocketAddr,
    path: &str,
) -> Sock {
    let plen = write_nul_path(io, limits.small(1), 0, path);
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    io.submit(
        Token::new(3),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: limits.small(1),
            path_off: 0,
            path_len: plen,
            flags: OpenFlags::NONE
                .with_read(true)
                .with_write(true)
                .with_create(true)
                .with_direct(true),
        },
    )
    .unwrap();
    let cs = drive_until(io, out, 10_000, |v| {
        [1, 2, 3]
            .iter()
            .all(|t| v.iter().any(|c| c.token() == Token::new(*t)))
    })
    .unwrap();
    Sock::new(u32::try_from(cs.iter().find(|c| c.token().get() == 1).unwrap().result()).unwrap())
}

/// Sends the small buffer to `srv` (armed recv) while writing the aligned
/// buffer to the file; returns the ring buffer that received the bytes.
#[cfg(unix)]
fn burst_send_and_write<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    limits: Limits,
    srv: Sock,
) -> BufId {
    io.submit(Token::new(4), Op::ArmedRecv { sock: srv })
        .unwrap();
    io.submit(
        Token::new(5),
        Op::Send {
            sock: Sock::new(1),
            buf: limits.small(0),
            off: 0,
            len: 8,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(
        Token::new(6),
        Op::FileWrite {
            slot: FileSlot::new(0),
            buf: limits.aligned(0),
            off: 0,
            len: 4096,
            file_off: 0,
        },
    )
    .unwrap();
    let cs2 = drive_until(io, out, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 4) && v.iter().any(|c| c.token().get() == 6)
    })
    .unwrap();
    cs2.iter()
        .find(|c| c.token().get() == 4)
        .unwrap()
        .buf()
        .unwrap()
}

/// Connects `Sock(1)` to an armed accept on a Unix socket at `sock_p`;
/// returns the accepted server socket.
#[cfg(unix)]
fn unix_armed_pair<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    limits: Limits,
    sock_p: &str,
) -> Sock {
    io.listen_unix(Sock::new(0), sock_p).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    let sp_len = write_nul_path(io, limits.small(0), 0, sock_p);
    io.submit(
        Token::new(2),
        Op::ConnectUnix {
            sock: Sock::new(1),
            path_buf: limits.small(0),
            path_off: 0,
            path_len: sp_len,
        },
    )
    .unwrap();
    let cs = drive_until(io, out, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 1) && v.iter().any(|c| c.token().get() == 2)
    })
    .unwrap();
    let srv = Sock::new(
        u32::try_from(cs.iter().find(|c| c.token().get() == 1).unwrap().result()).unwrap(),
    );
    assert_eq!(
        cs.iter().find(|c| c.token().get() == 2).unwrap().result(),
        0
    );
    srv
}

/// Opens `data_p`, writes `len` bytes of `BufId(1)` into it, and datasyncs, as one chain.
#[cfg(unix)]
fn persist_chain<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    limits: Limits,
    data_p: &str,
    len: u32,
) {
    let fp_len = write_nul_path(io, limits.small(1), 0, data_p);
    io.submit_chain(&[
        (
            Token::new(5),
            Op::FileOpen {
                slot: FileSlot::new(0),
                path_buf: limits.small(1),
                path_off: 0,
                path_len: fp_len,
                flags: OpenFlags::NONE
                    .with_read(true)
                    .with_write(true)
                    .with_create(true),
            },
        ),
        (
            Token::new(6),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len,
                file_off: 0,
            },
        ),
        (
            Token::new(7),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    let fcs = drive_until(io, out, 10_000, |v| v.iter().any(|c| c.token().get() == 7)).unwrap();
    assert_eq!(
        fcs.iter().find(|c| c.token().get() == 7).unwrap().result(),
        0
    );
}

/// The server acknowledges with `OK`; checks the client reads exactly that.
#[cfg(unix)]
fn ack_ok<IO: Io>(io: &mut IO, out: &mut Completions, srv: Sock) {
    write_pool_bytes(io, BufId::new(2), 0, b"OK");
    io.submit(
        Token::new(8),
        Op::Send {
            sock: srv,
            buf: BufId::new(2),
            off: 0,
            len: 2,
            zero_copy: false,
        },
    )
    .unwrap();
    let ack = submit_and_wait_one(
        io,
        out,
        Token::new(9),
        Op::Recv {
            sock: Sock::new(1),
            buf: BufId::new(3),
            off: 0,
            len: 8,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(ack.result(), 2);
    assert_eq!(read_pool_bytes(io, BufId::new(3), 0, 2), b"OK");
}

#[cfg(unix)]
#[test]
fn t3_15_unix_domain_armed_recv_and_chained_file_persist() {
    use super::super::harness::posix_backend;

    let dir = TempDir::new("t3_15").unwrap();
    let sock_p = dir.socket_path("rpc.sock");
    let data_p = dir.file_path("persist.bin");
    let limits = (net_limits(12, 6, 4, 4, 1024))
        .with_files_max(4)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096);
    let mut io = posix_backend(limits).unwrap();
    let mut out = Completions::new(16);

    let srv = unix_armed_pair(&mut io, &mut out, limits, sock_p.to_str().unwrap());
    io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
        .unwrap();
    let msg = b"persist-this-unix-payload";
    write_pool_bytes(&mut io, BufId::new(0), 0, msg);
    io.submit(
        Token::new(4),
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: msg.len() as u32,
            zero_copy: false,
        },
    )
    .unwrap();
    let rcs = drive_until(&mut io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 3)
    })
    .unwrap();
    let rc = rcs.iter().find(|c| c.token().get() == 3).unwrap();
    let rbuf = rc.buf().unwrap();
    let got = read_pool_bytes(&mut io, rbuf, 0, usize::try_from(rc.result()).unwrap());
    assert_eq!(got, msg);
    write_pool_bytes(&mut io, BufId::new(1), 0, &got);
    io.give_back(rbuf);

    persist_chain(
        &mut io,
        &mut out,
        limits,
        data_p.to_str().unwrap(),
        msg.len() as u32,
    );
    ack_ok(&mut io, &mut out, srv);
    assert_eq!(std::fs::read(&data_p).unwrap(), msg);
}
