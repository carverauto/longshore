// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-15`: `Posix` loopback S3 client object lifecycle (`kqueue` on macOS /
//! `epoll` on Linux) including `Budget` + `Report`, multipart, macOS `pin`
//! refusal, and `alloc_guard::sealed` steady-state checks.

use super::super::harness::HttpStoreServer;
#[cfg(target_os = "macos")]
use longshore::client::BuildError;
use longshore::client::{Budget, Buffer, Builder, Client};
use longshore::io::alloc_guard::sealed;
use longshore::store::creds::Credentials;
use longshore::store::report::{End, Expect, Put, Report};
use std::time::{Duration, Instant};

fn answered_status(rep: &Report) -> Option<u16> {
    match rep.end {
        End::Answered(a) => Some(a.status),
        End::NotSent(_) | End::Unknown(_) => None,
    }
}

fn creds() -> Credentials {
    Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .expect("creds")
}

/// The two-thread checksummed plain client the lifecycle runs against.
#[cfg(unix)]
fn lifecycle_client(store: &HttpStoreServer) -> Client {
    let mut b = Builder::new("127.0.0.1", "us-east-1", creds());
    b.bucket_in_path = Some("posix-bucket".into());
    b.tls = false;
    b.port = store.port();
    b.address = Some(store.addr());
    b.threads = 2;
    b.buffers = 4;
    b.buffer_size = 16 * 1024;
    b.connections = 4;
    b.operations = 8;
    b.waiters = 32;
    b.checksums = true;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.build().expect("Posix S3 client build")
}

/// Store, no-clobber twice, fetch, and ranged fetch with a pre-allocated
/// buffer under `alloc_guard::sealed`; returns the buffer.
#[cfg(unix)]
fn sealed_steady_state(client: &Client, body: &[u8]) -> Buffer {
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..body.len()].copy_from_slice(body);

    let (st_ok, nc_first, nc_second, fetch_len, range_len, buf) = sealed(|| {
        let (r, b1) = client.store("obj1", buf, body.len()).wait();
        let (n1, b2) = client.store_no_clobber("obj-nc", b1, body.len()).wait();
        let (n2, b3) = client.store_no_clobber("obj-nc", b2, body.len()).wait();
        let (f, b4) = client.fetch("obj1", b3).wait();
        let (fr, b5) = client.fetch_range("obj1", 6, 11, b4).wait();
        (
            r.is_ok(),
            n1.unwrap(),
            n2.unwrap(),
            f.unwrap(),
            fr.unwrap(),
            b5,
        )
    });
    assert!(st_ok);
    assert!(nc_first);
    assert!(!nc_second);
    assert_eq!(fetch_len, Some(body.len()));
    assert_eq!(range_len, Some(6));
    assert_eq!(&buf.as_slice()[..6], &body[6..=11]);
    buf
}

/// Uploads `AAAA` and `BBBB` as two parts and checks the joined object.
#[cfg(unix)]
fn multipart_lifecycle(client: &Client, store: &HttpStoreServer, mut buf: Buffer) -> Buffer {
    let up = client.upload_begin("mp-obj").wait().unwrap();
    buf.as_mut_slice()[..4].copy_from_slice(b"AAAA");
    let (p1, mut buf) = client.upload_part("mp-obj", &up, 1, buf, 4).wait();
    let p1 = p1.unwrap();
    buf.as_mut_slice()[..4].copy_from_slice(b"BBBB");
    let (p2, buf) = client.upload_part("mp-obj", &up, 2, buf, 4).wait();
    let p2 = p2.unwrap();
    let (done, buf) = client.upload_complete("mp-obj", &up, &[p1, p2], buf).wait();
    done.unwrap();
    assert_eq!(
        store.get_object("posix-bucket/mp-obj").unwrap().data,
        b"AAAABBBB"
    );
    buf
}

/// Exact `Budget` + `Report` calls: put, head, get, and erase.
#[cfg(unix)]
fn exact_calls(client: &Client, store: &HttpStoreServer, mut buf: Buffer) {
    let budget = Budget {
        attempts: 2,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    };
    buf.as_mut_slice()[..5].copy_from_slice(b"exact");
    let (put_rep, buf) = client
        .put("exact-key", buf, 5, &Put::default(), budget)
        .wait();
    let put_rep = put_rep.unwrap();
    assert_eq!(answered_status(&put_rep), Some(200));

    let head_rep = client
        .head("exact-key", &Expect::default(), budget)
        .wait()
        .unwrap();
    assert_eq!(answered_status(&head_rep), Some(200));

    let (get_rep, back) = client
        .get("exact-key", None, &Expect::default(), buf, budget)
        .wait();
    let get_rep = get_rep.unwrap();
    assert_eq!(answered_status(&get_rep), Some(200));
    assert_eq!(&back.as_slice()[..5], b"exact");

    client.erase("exact-key").wait().unwrap();
    assert!(store.get_object("posix-bucket/exact-key").is_none());
}

#[cfg(unix)]
#[test]
fn t4_15_posix_loopback_s3_client_full_object_lifecycle() {
    let store = HttpStoreServer::start_plain().unwrap();

    #[cfg(target_os = "macos")]
    {
        let mut bad = Builder::new("127.0.0.1", "us-east-1", creds());
        bad.pin = Some((0, 1));
        bad.address = Some(store.addr());
        bad.port = store.port();
        bad.tls = false;
        assert!(matches!(bad.build(), Err(BuildError::Setting(_))));
    }

    let client = lifecycle_client(&store);
    assert_eq!(client.threads(), 2);
    assert_eq!(client.buffer_size(), 16 * 1024);

    let buf = sealed_steady_state(&client, b"posix-kqueue-epoll-s3-object-data");
    let buf = multipart_lifecycle(&client, &store, buf);
    exact_calls(&client, &store, buf);
}
