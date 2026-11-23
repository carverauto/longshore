// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-04`: Plain no-TLS build (`--no-default-features`) + TLS rejection
//! (`BuildError::Setting` naming `"tls"`) + plain HTTP S3 & `Stream` workload.

use super::super::harness::HttpStoreServer;
#[cfg(feature = "sim")]
use super::super::harness::{drive_until, write_pool_bytes};
#[cfg(not(feature = "tls"))]
use longshore::client::BuildError;
use longshore::client::Builder;
#[cfg(feature = "sim")]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
use longshore::stream::TlsError;
#[cfg(feature = "sim")]
use longshore::stream::{Step, Stream};
use std::process::Command;

fn test_creds() -> Credentials {
    Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .expect("creds")
}

#[cfg(not(feature = "tls"))]
#[test]
fn t4_04_plain_build_rejects_tls_setting_naming_tls() {
    let creds = test_creds();
    let b = Builder::new("127.0.0.1", "us-east-1", creds)
        .tls(true)
        .port(443)
        .address(Some("127.0.0.1:443".parse().unwrap()))
        .portable(true);
    let err = match b.build() {
        Ok(_) => panic!("tls = true must fail when tls feature is disabled"),
        Err(e) => e,
    };
    match err {
        BuildError::Setting(msg) => {
            assert!(
                msg.contains("tls"),
                "BuildError::Setting must name \"tls\", got: {msg}"
            );
        }
        other @ (BuildError::Unsupported | BuildError::NoNameserver | BuildError::Io(_)) => {
            panic!("expected BuildError::Setting, got: {other:?}")
        }
    }
}

#[test]
fn t4_04_plain_http_s3_client_full_lifecycle_and_tls_error_accessible() {
    // TlsError must remain public and inspectable even when tls feature is off.
    let err_sample = TlsError::BadRecord;
    assert!(!format!("{err_sample:?}").is_empty());

    let store = HttpStoreServer::start_plain().unwrap();
    let b = Builder::new("127.0.0.1", "us-east-1", test_creds())
        .bucket_in_path(Some("plain-bucket".into()))
        .tls(false)
        .port(store.port())
        .address(Some(store.addr()))
        .threads(1)
        .buffers(4)
        .buffer_size(16 * 1024)
        .connections(4)
        .operations(8)
        .portable(true);
    let client = b.build().expect("plain S3 client build");

    let mut buf = client.buffer_blocking();
    let payload = b"plain-no-tls-s3-payload";
    buf.as_mut_slice()[..payload.len()].copy_from_slice(payload);
    let (res, buf) = client.store("alpha", buf, payload.len()).wait();
    res.expect("store");

    let (nc1, buf) = client.store_no_clobber("alpha", buf, payload.len()).wait();
    assert!(!nc1.unwrap());
    let (nc2, buf) = client.store_no_clobber("beta", buf, payload.len()).wait();
    assert!(nc2.unwrap());

    let (fr, mut buf) = client.fetch_range("alpha", 6, 11, buf).wait();
    let n = fr.unwrap().expect("range present");
    assert_eq!(&buf.as_slice()[..n], &payload[6..=11]);

    let up = client.upload_begin("multi").wait().unwrap();
    buf.as_mut_slice()[..5].copy_from_slice(b"part1");
    let (p1, mut buf) = client.upload_part("multi", &up, 1, buf, 5).wait();
    let p1 = p1.unwrap();
    buf.as_mut_slice()[..5].copy_from_slice(b"part2");
    let (p2, buf) = client.upload_part("multi", &up, 2, buf, 5).wait();
    let p2 = p2.unwrap();
    let (done, _buf) = client.upload_complete("multi", &up, &[p1, p2], buf).wait();
    done.unwrap();
    assert_eq!(
        store.get_object("plain-bucket/multi").unwrap().data,
        b"part1part2"
    );

    client.erase("alpha").wait().unwrap();
    assert!(store.get_object("plain-bucket/alpha").is_none());
}

#[cfg(feature = "sim")]
#[test]
fn t4_04_plain_stream_echo_without_tls() {
    let lim = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(4)
        .with_buffer_size(1024)
        .with_ops_max(8);
    let mut sim = SimIo::new(lim, 44, Faults::default());
    let mut out = Completions::new(8);
    let addr = sim
        .listen(Sock::new(0), "127.0.0.1:9404".parse().unwrap())
        .unwrap();
    sim.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    sim.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut sim, &mut out, 100, |v| v.len() >= 2).unwrap();

    let mut c_stream = Stream::new(Sock::new(1), Token::new(10), Token::new(11));
    let mut s_stream = Stream::new(Sock::new(2), Token::new(12), Token::new(13));
    write_pool_bytes(&mut sim, BufId::new(0), 0, b"stream-plain-hello");
    c_stream.send(&mut sim, BufId::new(0), 0, 18, u32::MAX);
    assert_eq!(s_stream.recv(&mut sim, BufId::new(1), 0, 64), Step::Pending);

    let mut sent = false;
    let mut got = 0usize;
    while !sent || got == 0 {
        sim.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 10 {
                if let Step::Done(n) = c_stream.on_send(&mut sim, c.result()) {
                    assert_eq!(n, 18);
                    sent = true;
                }
            } else if c.token().get() == 13 {
                if let Step::Done(n) = s_stream.on_recv(&mut sim, c.result()) {
                    assert!(n > 0);
                    got = usize::try_from(n).unwrap();
                }
            }
        }
    }
    assert_eq!(&sim.buf(BufId::new(1))[..got], b"stream-plain-hello");
}

#[test]
fn t4_04_cargo_tree_no_default_features_excludes_tls_and_aws_lc() {
    let manifest_dir = crate::e2e::harness::workspace_root().join("crates/longshore");
    let output = Command::new("cargo")
        .args(["tree", "--no-default-features", "-e", "normal"])
        .current_dir(manifest_dir)
        .output()
        .expect("invoke cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("longshore_tls"),
        "longshore --no-default-features must exclude longshore_tls:\n{stdout}"
    );
    assert!(
        !stdout.contains("aws-lc-sys"),
        "longshore --no-default-features must exclude aws-lc-sys:\n{stdout}"
    );
}
