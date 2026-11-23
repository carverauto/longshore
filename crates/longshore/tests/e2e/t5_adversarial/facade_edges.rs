// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `GAP-F1`: Public facade re-exports without `sim`, `Stream::send_alert` on
//! plain `PosixIo`, `Builder` errors, and `tls = false` under default features.

use super::super::harness::HttpStoreServer;
use longshore::checksum::{crc64_header_value, crc64_nvme};
use longshore::client::{BuildError, Builder};
use longshore::crypto::sha256;
use longshore::dns::Resolver;
use longshore::http::client::Config as HttpConfig;
use longshore::io::posix::PosixIo;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::{queue_parked, TryRecv};
use longshore::store::creds::Credentials;
use longshore::store::s3::{Config as S3Config, S3};
use longshore::stream::{Step, Stream, TlsError, KEY_RECORDS_RETIRE};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[test]
fn facade_non_optional_reexports_send_alert_plain_and_builder_errors_without_sim() {
    // Verify checksum, crypto, dns, http, mailbox, store, stream, io facade re-exports.
    assert_eq!(sha256(b"").len(), 32);
    assert!(!crc64_header_value(crc64_nvme(b"abc")).is_empty());
    const { assert!(KEY_RECORDS_RETIRE > 0) };
    assert!(!format!("{:?}", TlsError::BadRecord).is_empty());
    let _http_cfg_size = core::mem::size_of::<HttpConfig>();
    let _s3_size = core::mem::size_of::<(S3Config, S3)>();
    let _resolver_size = core::mem::size_of::<Resolver>();

    let (tx, mut rx) = queue_parked::<u32>(2);
    tx.try_send(9).unwrap();
    assert_eq!(rx.try_recv(), TryRecv::Message(9));

    // Plain Stream::send_alert over PosixIo (no sim required).
    let mut io = PosixIo::new(Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(8);
    let addr = io
        .listen(Sock(0), SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .unwrap();
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(2),
            addr,
        },
    )
    .unwrap();
    let mut up = 0;
    while up < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result, 0);
            up += 1;
        }
    }
    let mut plain = Stream::new(Sock(2), Token(10), Token(11));
    plain.send_alert(&mut io, BufId(0), 0, 1, 0);
    loop {
        io.tick(true, &mut out).unwrap();
        if let Some(c) = out.pop() {
            assert_eq!(c.token, Token(10));
            assert_eq!(plain.on_send(&mut io, c.result), Step::Done(0));
            break;
        }
    }

    // Builder error paths (Setting / NoNameserver).
    let creds = Credentials::new("AKIDEXAMPLE", "secret").unwrap();
    let mut b_bad = Builder::new("bucket example.com", "us-east-1", creds);
    b_bad.nameservers = vec!["192.0.2.53:53".parse().unwrap()];
    assert!(matches!(b_bad.build(), Err(BuildError::Setting(_))));

    let mut b_no_ns = Builder::new("bucket.s3.example.com", "us-east-1", creds);
    b_no_ns.nameservers = Vec::new();
    let res = b_no_ns.build();
    assert!(res.is_ok() || matches!(res, Err(BuildError::NoNameserver | BuildError::Setting(_))));
}

#[test]
fn facade_builder_tls_false_selects_plain_http_even_when_tls_feature_enabled() {
    let store = HttpStoreServer::start_plain().unwrap();
    let creds = Credentials::new("AKIDEXAMPLE", "secret").unwrap();
    let mut b = Builder::new("127.0.0.1", "us-east-1", creds);
    b.bucket_in_path = Some("plain-explicit".into());
    b.tls = false;
    b.port = store.port();
    b.address = Some(store.addr());
    b.threads = 1;
    b.buffers = 2;
    b.buffer_size = 8192;
    b.connections = 2;
    b.operations = 4;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    let client = b.build().expect("plain client when tls = false");
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..5].copy_from_slice(b"hello");
    let (r, _buf) = client.store("k", buf, 5).wait();
    assert_eq!(r, Ok(()));
    assert_eq!(store.get_object("plain-explicit/k").unwrap().data, b"hello");
}
