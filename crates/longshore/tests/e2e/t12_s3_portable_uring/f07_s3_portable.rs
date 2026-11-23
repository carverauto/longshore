// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Feature 7: S3 client portable backend selection (`#41`).

use crate::e2e::harness::HttpStoreServer;
use longshore::client::{BuildError, Builder};
use longshore::store::creds::Credentials;

fn test_creds() -> Credentials {
    Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .expect("creds")
}

fn portable_builder(server: &HttpStoreServer) -> Builder {
    Builder::new("127.0.0.1", "us-east-1", test_creds())
        .port(server.port())
        .address(Some(server.addr()))
        .bucket_in_path(Some("test-bucket".into()))
        .tls(false)
        .buffers(4)
        .buffer_size(16 * 1024)
        .connections(4)
        .operations(8)
        .portable(true)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn t1_f07_1_portable_s3_store_and_fetch_loopback() {
    let server = HttpStoreServer::start_plain().expect("server");
    let client = portable_builder(&server).build().expect("portable client");

    let payload = b"portable-backend-object-body";
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..payload.len()].copy_from_slice(payload);
    let (stored, buf) = client.store("obj-1", buf, payload.len()).wait();
    stored.expect("store");

    let (fetched, buf) = client.fetch("obj-1", buf).wait();
    let len = fetched.expect("fetch").expect("present");
    assert_eq!(&buf.as_slice()[..len], payload);

    let obj = server
        .get_object("test-bucket/obj-1")
        .expect("server object");
    assert_eq!(obj.data, payload);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn t1_f07_2_portable_multithread_store_fetch_range_erase() {
    let server = HttpStoreServer::start_plain().expect("server");
    let b = portable_builder(&server).threads(2);
    let client = b.build().expect("2-thread portable client");

    let handles: Vec<_> = (0..2)
        .map(|idx| {
            let c = client.clone();
            std::thread::spawn(move || {
                let key = format!("mt-key-{idx}");
                let data = format!("0123456789-thread-{idx}");
                let mut buf = c.buffer_blocking();
                buf.as_mut_slice()[..data.len()].copy_from_slice(data.as_bytes());
                let (st, buf) = c.store(&key, buf, data.len()).wait();
                st.expect("store");

                let (rg, buf) = c.fetch_range(&key, 3, 9, buf).wait();
                let n = rg.expect("range").expect("exists");
                assert_eq!(&buf.as_slice()[..n], &data.as_bytes()[3..=9]);

                c.erase(&key).wait().expect("erase");
                let (gone, _) = c.fetch(&key, buf).wait();
                assert_eq!(gone.expect("fetch after erase"), None);
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread join");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f07_3_linux_default_portable_false_uses_uring() {
    let server = HttpStoreServer::start_plain().expect("server");
    let b = portable_builder(&server).portable(false);
    let default_b = Builder::new("127.0.0.1", "us-east-1", test_creds());
    assert!(
        !default_b.is_portable(),
        "Linux Builder must default portable to false"
    );

    let client = b.build().expect("uring client");
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..10].copy_from_slice(b"uring-data");
    let (st, buf) = client.store("uring-key", buf, 10).wait();
    st.expect("store");
    let (ft, buf) = client.fetch("uring-key", buf).wait();
    assert_eq!(ft.expect("fetch"), Some(10));
    assert_eq!(&buf.as_slice()[..10], b"uring-data");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn t1_f07_4_portable_multipart_upload_lifecycle() {
    let server = HttpStoreServer::start_plain().expect("server");
    let client = portable_builder(&server).build().expect("client");

    let upload = client.upload_begin("multi.bin").wait().expect("begin");
    let mut parts = Vec::new();
    let p1 = b"part-one-bytes-";
    let p2 = b"part-two-bytes";

    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..p1.len()].copy_from_slice(p1);
    let (r1, mut buf) = client
        .upload_part("multi.bin", &upload, 1, buf, p1.len())
        .wait();
    parts.push(r1.expect("part 1"));

    buf.as_mut_slice()[..p2.len()].copy_from_slice(p2);
    let (r2, buf) = client
        .upload_part("multi.bin", &upload, 2, buf, p2.len())
        .wait();
    parts.push(r2.expect("part 2"));

    let (done, buf) = client
        .upload_complete("multi.bin", &upload, &parts, buf)
        .wait();
    done.expect("complete");

    let (fetched, buf) = client.fetch("multi.bin", buf).wait();
    let n = fetched.expect("fetch").expect("exists");
    let mut expected = Vec::new();
    expected.extend_from_slice(p1);
    expected.extend_from_slice(p2);
    assert_eq!(&buf.as_slice()[..n], expected.as_slice());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn t1_f07_5_portable_client_accessors_and_no_checksums() {
    let server = HttpStoreServer::start_plain().expect("server");
    let b = portable_builder(&server).threads(2).buffer_size(16 * 1024);
    let client = b.build().expect("client").with_checksums(false);
    assert_eq!(client.threads(), 2);
    assert_eq!(client.buffer_size(), 16 * 1024);

    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..7].copy_from_slice(b"nocheck");
    let (st, buf) = client.store("nc-key", buf, 7).wait();
    st.expect("store without checksum");
    let (ft, buf) = client.fetch("nc-key", buf).wait();
    assert_eq!(ft.expect("fetch"), Some(7));
    assert_eq!(&buf.as_slice()[..7], b"nocheck");
}

#[cfg(target_os = "macos")]
#[test]
fn t2_f07_1_macos_pin_returns_setting_error() {
    let server = HttpStoreServer::start_plain().expect("server");
    let b = portable_builder(&server).pin(Some((0, 1)));
    match b.build() {
        Err(BuildError::Setting(_)) => {}
        Err(other) => panic!("expected BuildError::Setting, got {other:?}"),
        Ok(_) => panic!("pinning on macOS must fail"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn t2_f07_2_linux_uring_refused_returns_io_error_without_fallback() {
    const ENV_KEY: &str = "LONGSHORE_E2E_CHILD_NOFILE";
    if std::env::var_os(ENV_KEY).is_some() {
        #[repr(C)]
        struct RLimit {
            rlim_cur: u64,
            rlim_max: u64,
        }
        extern "C" {
            fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
        }
        const RLIMIT_NOFILE: i32 = 7;
        let limit = RLimit {
            rlim_cur: 8,
            rlim_max: 8,
        };
        let rc = unsafe { setrlimit(RLIMIT_NOFILE, &limit) };
        assert_eq!(rc, 0, "setrlimit failed");

        let b = Builder::new("127.0.0.1", "us-east-1", test_creds())
            .address(Some("127.0.0.1:80".parse().unwrap()))
            .connections(64)
            .buffers(4)
            .buffer_size(8192)
            .portable(false);
        match b.build() {
            Err(BuildError::Io(e)) => assert!(!e.to_string().is_empty()),
            Err(other) => panic!("expected BuildError::Io, got {other:?}"),
            Ok(_) => panic!("must fail when io_uring is refused, never fall back to Posix"),
        }
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "e2e::t12_s3_portable_uring::f07_s3_portable::t2_f07_2_linux_uring_refused_returns_io_error_without_fallback",
            "--nocapture",
        ])
        .env(ENV_KEY, "1")
        .output()
        .expect("spawn child");
    assert!(
        out.status.success(),
        "child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn t2_f07_3_builder_invalid_threads_or_buffer_size() {
    let server = HttpStoreServer::start_plain().expect("server");

    let b0 = portable_builder(&server).threads(0);
    assert!(matches!(b0.build(), Err(BuildError::Setting(_))));

    let b_hi = portable_builder(&server).threads(1025);
    assert!(matches!(b_hi.build(), Err(BuildError::Setting(_))));

    let b_small = portable_builder(&server).buffer_size(4096);
    assert!(matches!(b_small.build(), Err(BuildError::Setting(_))));

    let b_conn = portable_builder(&server).connections(0);
    assert!(matches!(b_conn.build(), Err(BuildError::Setting(_))));
}

#[test]
fn t2_f07_4_builder_invalid_host_or_nameservers() {
    let b_empty =
        Builder::new("", "us-east-1", test_creds()).address(Some("127.0.0.1:80".parse().unwrap()));
    assert!(matches!(b_empty.build(), Err(BuildError::Setting(_))));

    let b_bad = Builder::new("bad host!", "us-east-1", test_creds());
    assert!(matches!(b_bad.build(), Err(BuildError::Setting(_))));

    let b_ns = Builder::new("example.com", "us-east-1", test_creds()).nameservers(vec![
        "127.0.0.1:53".parse().unwrap(),
        "127.0.0.2:53".parse().unwrap(),
        "127.0.0.3:53".parse().unwrap(),
    ]);
    assert!(matches!(b_ns.build(), Err(BuildError::Setting(_))));

    let b_reg = Builder::new("example.com", &"r".repeat(65), test_creds())
        .address(Some("127.0.0.1:80".parse().unwrap()));
    assert!(matches!(b_reg.build(), Err(BuildError::Setting(_))));
}

#[test]
fn t2_f07_5_builder_invalid_bucket_in_path() {
    let server = HttpStoreServer::start_plain().expect("server");
    for bad in ["AB", "ab", "MyBucket", "bad_bucket"] {
        let b = portable_builder(&server).bucket_in_path(Some(bad.into()));
        assert!(
            matches!(b.build(), Err(BuildError::Setting(_))),
            "expected BuildError::Setting for bucket_in_path={bad:?}"
        );
    }
}
