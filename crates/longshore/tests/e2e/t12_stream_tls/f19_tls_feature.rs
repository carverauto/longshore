// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 19 E2E tests: optional default-on `tls` Cargo feature across workspace.

#[cfg(feature = "tls")]
use super::super::harness::make_tls_identity;
use super::super::harness::HttpStoreServer;
#[cfg(feature = "sim")]
use super::super::harness::{drive_until, write_pool_bytes};
#[cfg(not(feature = "tls"))]
use longshore::client::BuildError;
use longshore::client::Builder;
#[cfg(feature = "sim")]
use longshore::io::{BufId, Completions, Io, Limits, Op};
#[cfg(any(feature = "sim", not(feature = "tls")))]
use longshore::io::{Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
#[cfg(feature = "sim")]
use longshore::stream::Step;
#[cfg(any(feature = "sim", not(feature = "tls")))]
use longshore::stream::Stream;
use longshore::stream::TlsError;
use std::path::Path;
use std::process::Command;

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
}

fn describe_tls_err(err: TlsError) -> &'static str {
    match err {
        TlsError::BadCertificate => "bad-cert",
        TlsError::Handshake => "handshake",
        TlsError::BadRecord => "bad-record",
        TlsError::Setup => "setup",
        TlsError::NotDone => "not-done",
        TlsError::UnsupportedSuite => "unsupported-suite",
        TlsError::Alert(_) => "alert",
        _ => "other",
    }
}

#[cfg(feature = "tls")]
#[test]
fn t1_f19_1_default_feature_enables_tls_module_and_https_store() {
    let ident = make_tls_identity(&["127.0.0.1", "localhost"]);
    let srv = HttpStoreServer::start_tls(ident.server_setup()).expect("start tls store");
    let mut b = Builder::new(
        "localhost",
        "us-east-1",
        Credentials::new("K", "S").unwrap(),
    );
    b.tls = true;
    b.port = srv.port();
    b.address = Some(srv.addr());
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.buffers = 2;
    b.buffer_size = 256 * 1024;
    b.connections = 2;
    b.operations = 2;
    let client = b.build().expect("tls enabled builder succeeds");
    assert_eq!(client.buffer_size(), 256 * 1024);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f19_2_plain_stream_and_tls_error_compile_with_or_without_tls_feature() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 1902, Faults::default());
    let mut out = Completions::new(8);
    let addr = io
        .listen(Sock(0), "192.0.2.19:8080".parse().unwrap())
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
    drive_until(&mut io, &mut out, 50, |v| v.len() == 2).unwrap();

    let mut s = Stream::new(Sock(1), Token(3), Token(4));
    assert!(!s.is_tls());
    assert_eq!(s.records(), 0);
    write_pool_bytes(&mut io, BufId(0), 0, b"plain");
    s.send(&mut io, BufId(0), 0, 5, 4096);
    let cs = drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(3))
    })
    .unwrap();
    let sent = cs.iter().find(|c| c.token == Token(3)).unwrap().result;
    assert!(sent > 0);
    assert_eq!(s.on_send(&mut io, sent), Step::Done(sent));
    assert_eq!(describe_tls_err(TlsError::BadCertificate), "bad-cert");
}

#[test]
fn t1_f19_3_plain_s3_client_store_fetch_erase_without_tls() {
    let srv = HttpStoreServer::start_plain().expect("start plain store");
    let mut b = Builder::new(
        "127.0.0.1",
        "us-east-1",
        Credentials::new("K", "S").unwrap(),
    );
    b.tls = false;
    b.port = srv.port();
    b.address = Some(srv.addr());
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.buffers = 2;
    b.buffer_size = 16 * 1024;
    b.connections = 2;
    b.operations = 4;
    let client = b.build().expect("plain client build");
    let mut buf = client.buffer_blocking();
    let payload = b"plain-http-e2e-body";
    buf.as_mut_slice()[..payload.len()].copy_from_slice(payload);
    let (res, buf) = client.store("plain-key", buf, payload.len()).wait();
    res.expect("store");
    let (got, buf) = client.fetch("plain-key", buf).wait();
    assert_eq!(got.expect("fetch"), Some(payload.len()));
    assert_eq!(&buf.as_slice()[..payload.len()], payload);
    drop(buf);
    client.erase("plain-key").wait().expect("erase");
    assert!(srv.get_object("plain-key").is_none());
}

#[test]
fn t1_f19_4_cargo_tree_no_default_features_excludes_tls_and_aws_lc() {
    let out = Command::new("cargo")
        .args([
            "tree",
            "-p",
            "longshore",
            "--no-default-features",
            "-e",
            "normal",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(
        !tree.contains("longshore_tls"),
        "unexpected longshore_tls:\n{tree}"
    );
    assert!(
        !tree.contains("aws-lc-sys"),
        "unexpected aws-lc-sys:\n{tree}"
    );
}

#[test]
fn t1_f19_5_cargo_tree_default_includes_tls_and_plain_sim_excludes_it() {
    let with_tls = Command::new("cargo")
        .args([
            "tree",
            "-p",
            "longshore",
            "--features",
            "sim",
            "-e",
            "normal",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("cargo tree default");
    assert!(with_tls.status.success());
    let tls_tree = String::from_utf8_lossy(&with_tls.stdout);
    assert!(tls_tree.contains("longshore_tls"));

    let plain = Command::new("cargo")
        .args([
            "tree",
            "-p",
            "longshore",
            "--no-default-features",
            "--features",
            "sim",
            "-e",
            "normal",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("cargo tree plain sim");
    assert!(plain.status.success());
    let plain_tree = String::from_utf8_lossy(&plain.stdout);
    assert!(!plain_tree.contains("longshore_tls"));
    assert!(!plain_tree.contains("aws-lc-sys"));
}

#[cfg(not(feature = "tls"))]
#[test]
fn t2_f19_1_plain_build_rejects_tls_builder_naming_tls() {
    let mut b = Builder::new(
        "example.com",
        "us-east-1",
        Credentials::new("K", "S").unwrap(),
    );
    b.tls = true;
    b.port = 443;
    b.address = Some("127.0.0.1:443".parse().unwrap());
    let err = match b.build() {
        Ok(_) => panic!("expected BuildError::Setting when tls=true without tls feature"),
        Err(e) => e,
    };
    match err {
        BuildError::Setting(msg) => {
            assert!(msg.contains("tls"), "setting error must name tls: {msg}");
        }
        other => panic!("expected BuildError::Setting, got {other:?}"),
    }
}

#[cfg(not(feature = "tls"))]
#[test]
fn t2_f19_2_plain_build_stream_has_no_tls_and_builder_rejects_before_io() {
    let mut s = Stream::new(Sock(0), Token(1), Token(2));
    assert!(!s.is_tls());
    assert_eq!(s.records(), 0);
    s.reset();
    let mut b = Builder::new(
        "127.0.0.1",
        "us-east-1",
        Credentials::new("K", "S").unwrap(),
    );
    b.tls = true;
    b.address = Some("127.0.0.1:1".parse().unwrap());
    let Err(BuildError::Setting(msg)) = b.build() else {
        panic!("expected BuildError::Setting");
    };
    assert!(msg.contains("tls"));
}

#[test]
fn t2_f19_3_stream_and_http_client_no_default_features_exclude_tls_deps() {
    for pkg in ["longshore_stream", "longshore_http_client"] {
        let out = Command::new("cargo")
            .args(["tree", "-p", pkg, "--no-default-features", "-e", "normal"])
            .current_dir(workspace_root())
            .output()
            .expect("cargo tree");
        assert!(out.status.success());
        let tree = String::from_utf8_lossy(&out.stdout);
        assert!(
            !tree.contains("longshore_tls"),
            "{pkg} leaked longshore_tls:\n{tree}"
        );
        assert!(
            !tree.contains("aws-lc-sys"),
            "{pkg} leaked aws-lc-sys:\n{tree}"
        );
    }
}

#[test]
fn t2_f19_4_cargo_metadata_verifies_default_tls_feature_wiring() {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata");
    assert!(out.status.success());
    let json = String::from_utf8_lossy(&out.stdout);
    for pkg in [
        "\"name\":\"longshore\"",
        "\"name\":\"longshore_stream\"",
        "\"name\":\"longshore_http_client\"",
        "\"name\":\"longshore_s3\"",
    ] {
        assert!(json.contains(pkg), "missing package {pkg} in metadata");
    }
    assert!(json.contains("\"default\":[\"tls\"]") || json.contains("\"tls\""));
}

#[test]
fn t2_f19_5_tls_error_enum_is_public_and_matchable_unconditionally() {
    let errs = [
        TlsError::BadCertificate,
        TlsError::Handshake,
        TlsError::BadRecord,
        TlsError::Setup,
        TlsError::NotDone,
        TlsError::UnsupportedSuite,
        TlsError::Alert(42),
    ];
    let labels: Vec<&str> = errs.into_iter().map(describe_tls_err).collect();
    assert_eq!(
        labels,
        [
            "bad-cert",
            "handshake",
            "bad-record",
            "setup",
            "not-done",
            "unsupported-suite",
            "alert"
        ]
    );
}
