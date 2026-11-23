// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-14`: Linux real-kernel `io_uring` S3 client & `UringIo` readiness check
//! (`portable = false` default on Linux, no silent fallback when `RLIMIT_MEMLOCK`
//! blocks ring registration, plus real-kernel S3 loopback store/fetch).

use super::super::harness::HttpStoreServer;
use longshore::client::{BuildError, Builder};
use longshore::store::creds::Credentials;

fn creds() -> Credentials {
    Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .expect("creds")
}

#[test]
fn t4_14_builder_defaults_and_linux_no_silent_fallback_contract() {
    let store = HttpStoreServer::start_plain().unwrap();
    let mut b = Builder::new("127.0.0.1", "us-east-1", creds());
    b.bucket_in_path = Some("uring-bucket".into());
    b.tls = false;
    b.port = store.port();
    b.address = Some(store.addr());
    b.buffers = 4;
    b.buffer_size = 16 * 1024;
    b.connections = 4;
    b.operations = 8;
    b.waiters = 32;

    // On Linux, Builder::new defaults portable = false (uses UringIo).
    #[cfg(target_os = "linux")]
    {
        assert!(!b.portable, "Linux Builder must default portable = false");
        let client = b.clone().build().expect("Linux UringIo S3 client build");
        let mut buf = client.buffer_blocking();
        buf.as_mut_slice()[..9].copy_from_slice(b"uring-s3!");
        let (s_res, buf) = client.store("uring-key", buf, 9).wait();
        s_res.expect("UringIo S3 store");
        let (f_res, back) = client.fetch("uring-key", buf).wait();
        assert_eq!(f_res.unwrap(), Some(9));
        assert_eq!(&back.as_slice()[..9], b"uring-s3!");
    }

    // Requesting portable = false with a buffer exceeding UringIo's 1 GiB
    // registered-range limit fails with BuildError::Io (never silent fallback to Posix).
    #[cfg(target_os = "linux")]
    {
        let mut huge = b;
        huge.buffers = 1;
        huge.buffer_size = (1 << 30) + 4096;
        match huge.build() {
            Err(BuildError::Io(_)) | Err(BuildError::Setting(_)) => {}
            Ok(_) => panic!("excessive UringIo client pool must fail rather than fall back"),
            Err(other) => panic!("unexpected BuildError: {other:?}"),
        }
    }

    // Non-Linux host (e.g. macOS) uses portable backend automatically and rejects pin.
    #[cfg(target_os = "macos")]
    {
        let mut pinned = b;
        pinned.pin = Some((0, 1));
        match pinned.build() {
            Err(BuildError::Setting(_)) => {}
            Ok(_) => panic!("macOS portable client must refuse pin"),
            Err(other) => panic!("unexpected BuildError on macOS pin: {other:?}"),
        }
    }
}
