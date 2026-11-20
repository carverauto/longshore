// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-13`–`T3-15`: portable `Client` + `Stream` server,
//! 4-buffer-class isolation under CQ bursts, and Unix-domain armed `Recv` + file chain.

#[allow(unused_imports)]
use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes, TempDir,
};
#[allow(unused_imports)]
use super::net_limits;
#[allow(unused_imports)]
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
    let mut b = Builder::new("127.0.0.1", "us-east-1", creds);
    b.port = server.port();
    b.address = Some(server.addr());
    b.bucket_in_path = Some("test-bucket".into());
    b.tls = false;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.threads = 1;
    b.buffers = 4;
    b.buffer_size = 64 * 1024;
    b.connections = 4;
    b.operations = 4;
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
        let mut bt = Builder::new("127.0.0.1", "us-east-1", creds);
        bt.port = tls_srv.port();
        bt.address = Some(tls_srv.addr());
        bt.bucket_in_path = Some("test-bucket".into());
        bt.tls = true;
        #[cfg(target_os = "linux")]
        {
            bt.portable = true;
        }
        bt.threads = 1;
        bt.buffers = 4;
        bt.buffer_size = 64 * 1024;
        bt.connections = 4;
        bt.operations = 4;
        match bt.build() {
            Ok(c) => {
                let mut b2 = c.buffer_blocking();
                b2.as_mut_slice()[..4].copy_from_slice(b"tls!");
                let _ = c.store("k-tls", b2, 4).wait();
            }
            Err(BuildError::Setting(msg)) => panic!("unexpected setting error: {msg}"),
            Err(e) => panic!("unexpected build error: {e}"),
        }
    }
}

#[test]
fn t3_14_four_buffer_classes_isolation_and_cq_burst() {
    #[allow(clippy::needless_update)]
    let limits = Limits {
        sockets_max: 32,
        buffers: 2,
        buffer_size: 8192,
        small_buffers: 4,
        small_buffer_size: 512,
        files_max: 16,
        aligned_buffers: 4,
        aligned_buffer_size: 4096,
        block_size: 4096,
        accept_range_start: 16,
        accept_range_len: 16,
        ring_buffers: 256,
        ring_buffer_size: 1024,
        ops_max: 32,
        ..Limits::BASE
    };

    #[cfg(unix)]
    {
        use super::super::harness::posix_backend;

        let dir = TempDir::new("t3_14").unwrap();
        let mut io = posix_backend(limits).unwrap();
        let mut out = Completions::new(64);
        let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();

        io.buf(BufId(0))[..8].copy_from_slice(b"LARGE-00");
        io.buf(limits.small(0))[..8].copy_from_slice(b"SMALL-00");
        io.buf(limits.aligned(0))[..8].copy_from_slice(b"ALIGN-00");
        assert_eq!(&io.buf(BufId(0))[..8], b"LARGE-00");
        assert_eq!(&io.buf(limits.small(0))[..8], b"SMALL-00");
        assert_eq!(&io.buf(limits.aligned(0))[..8], b"ALIGN-00");

        let plen = write_nul_path(
            &mut io,
            limits.small(1),
            0,
            dir.file_path("burst.dat").to_str().unwrap(),
        );
        io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .unwrap();
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr,
            },
        )
        .unwrap();
        io.submit(
            Token(3),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: limits.small(1),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    direct: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 10_000, |v| {
            [1, 2, 3]
                .iter()
                .all(|t| v.iter().any(|c| c.token == Token(*t)))
        })
        .unwrap();
        let srv = Sock(cs.iter().find(|c| c.token == Token(1)).unwrap().result as u32);
        io.submit(Token(4), Op::ArmedRecv { sock: srv }).unwrap();
        io.submit(
            Token(5),
            Op::Send {
                sock: Sock(1),
                buf: limits.small(0),
                off: 0,
                len: 8,
                zero_copy: false,
            },
        )
        .unwrap();
        io.submit(
            Token(6),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: limits.aligned(0),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        )
        .unwrap();
        let cs2 = drive_until(&mut io, &mut out, 10_000, |v| {
            v.iter().any(|c| c.token == Token(4)) && v.iter().any(|c| c.token == Token(6))
        })
        .unwrap();
        let rbuf = cs2
            .iter()
            .find(|c| c.token == Token(4))
            .unwrap()
            .buf
            .unwrap();
        assert_eq!(read_pool_bytes(&mut io, rbuf, 0, 8), b"SMALL-00");
        io.give_back(rbuf);
        assert_eq!(&io.buf(BufId(0))[..8], b"LARGE-00");
        assert_eq!(&io.buf(limits.aligned(0))[..8], b"ALIGN-00");
    }
}

#[cfg(unix)]
#[test]
fn t3_15_unix_domain_armed_recv_and_chained_file_persist() {
    use super::super::harness::posix_backend;

    let dir = TempDir::new("t3_15").unwrap();
    let sock_p = dir.socket_path("rpc.sock");
    let data_p = dir.file_path("persist.bin");
    #[allow(clippy::needless_update)]
    let limits = Limits {
        files_max: 4,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ..net_limits(12, 6, 4, 4, 1024)
    };
    let mut io = posix_backend(limits).unwrap();
    let mut out = Completions::new(16);

    io.listen_unix(Sock(0), sock_p.to_str().unwrap()).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    let sp_len = write_nul_path(&mut io, limits.small(0), 0, sock_p.to_str().unwrap());
    io.submit(
        Token(2),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: limits.small(0),
            path_off: 0,
            path_len: sp_len,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    let srv = Sock(cs.iter().find(|c| c.token == Token(1)).unwrap().result as u32);
    assert_eq!(cs.iter().find(|c| c.token == Token(2)).unwrap().result, 0);

    io.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();
    let msg = b"persist-this-unix-payload";
    write_pool_bytes(&mut io, BufId(0), 0, msg);
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: msg.len() as u32,
            zero_copy: false,
        },
    )
    .unwrap();
    let rcs = drive_until(&mut io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(3))
    })
    .unwrap();
    let rc = rcs.iter().find(|c| c.token == Token(3)).unwrap();
    let rbuf = rc.buf.unwrap();
    let got = read_pool_bytes(&mut io, rbuf, 0, rc.result as usize);
    assert_eq!(got, msg);
    write_pool_bytes(&mut io, BufId(1), 0, &got);
    io.give_back(rbuf);

    let fp_len = write_nul_path(&mut io, limits.small(1), 0, data_p.to_str().unwrap());
    io.submit_chain(&[
        (
            Token(5),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: limits.small(1),
                path_off: 0,
                path_len: fp_len,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    ..Default::default()
                },
            },
        ),
        (
            Token(6),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: msg.len() as u32,
                file_off: 0,
            },
        ),
        (
            Token(7),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    let fcs = drive_until(&mut io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(7))
    })
    .unwrap();
    assert_eq!(fcs.iter().find(|c| c.token == Token(7)).unwrap().result, 0);

    write_pool_bytes(&mut io, BufId(2), 0, b"OK");
    io.submit(
        Token(8),
        Op::Send {
            sock: srv,
            buf: BufId(2),
            off: 0,
            len: 2,
            zero_copy: false,
        },
    )
    .unwrap();
    let ack = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(9),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(3),
            off: 0,
            len: 8,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(ack.result, 2);
    assert_eq!(read_pool_bytes(&mut io, BufId(3), 0, 2), b"OK");
    assert_eq!(std::fs::read(&data_p).unwrap(), msg);
}
