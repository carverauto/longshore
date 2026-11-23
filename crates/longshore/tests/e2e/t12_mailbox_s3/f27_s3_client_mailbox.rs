// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 27 E2E tests: `longshore_s3::client` rebuilt on `longshore_mailbox`.

use super::super::harness::HttpStoreServer;
use longshore::client::{Budget, Builder, Error};
use longshore::io::alloc_guard::sealed;
#[cfg(all(feature = "fault-injection", target_os = "linux"))]
use longshore::io::uring::Fault;
use longshore::mailbox::block_on;
use longshore::store::creds::Credentials;
use longshore::store::report::{End, Expect, Put, Report};
use std::process::Command;
use std::sync::Arc;
use std::thread;

fn test_builder(srv: &HttpStoreServer, threads: u32, ops: u32, bufs: u32, waiters: u32) -> Builder {
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
    b.threads = threads;
    b.connections = 4;
    b.buffers = bufs;
    b.buffer_size = 16 * 1024;
    b.operations = ops;
    b.waiters = waiters;
    b
}

fn report_status(rep: &Report) -> Option<u16> {
    match rep.end {
        End::Answered(ans) => Some(ans.status),
        End::NotSent(_) | End::Unknown(_) => None,
    }
}

#[test]
fn t1_f27_1_builder_waiters_field_and_full_object_and_exact_report_api() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = test_builder(&srv, 1, 4, 2, 128).build().expect("build");

    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..7].copy_from_slice(b"mailbox");
    let (res, buf) = client.store("k1", buf, 7).wait();
    res.expect("store");
    let (got, buf) = client.fetch("k1", buf).wait();
    assert_eq!(got.expect("fetch"), Some(7));
    assert_eq!(&buf.as_slice()[..7], b"mailbox");

    // Exact Put/Head/Get with Budget + Report.
    let (rep, buf) = client
        .put("k2", buf, 7, &Put::default(), Budget::UNLIMITED)
        .wait();
    assert_eq!(report_status(&rep.expect("put report")), Some(200));
    let head_rep = client
        .head("k2", &Expect::default(), Budget::UNLIMITED)
        .wait()
        .expect("head");
    assert_eq!(report_status(&head_rep), Some(200));
    let (get_rep, buf) = client
        .get("k2", None, &Expect::default(), buf, Budget::UNLIMITED)
        .wait();
    assert_eq!(report_status(&get_rep.expect("get report")), Some(200));
    drop(buf);

    client.erase("k1").wait().expect("erase k1");
    client.erase("k2").wait().expect("erase k2");
}

#[test]
fn t1_f27_2_blocking_and_future_callers_share_lanes_concurrently() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = Arc::new(test_builder(&srv, 2, 4, 4, 64).build().expect("build"));

    let mut handles = Vec::new();
    for i in 0..8usize {
        let c = Arc::clone(&client);
        handles.push(thread::spawn(move || {
            let key = format!("conc-{i}");
            let mut buf = if i % 2 == 0 {
                c.buffer_blocking()
            } else {
                block_on(c.buffer())
            };
            let payload = format!("val-{i}");
            buf.as_mut_slice()[..payload.len()].copy_from_slice(payload.as_bytes());
            let (res, buf) = if i % 2 == 0 {
                c.store(&key, buf, payload.len()).wait()
            } else {
                block_on(c.store(&key, buf, payload.len()))
            };
            res.expect("store");
            let (got, buf) = block_on(c.fetch(&key, buf));
            assert_eq!(got.unwrap(), Some(payload.len()));
            assert_eq!(&buf.as_slice()[..payload.len()], payload.as_bytes());
            drop(buf);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn t1_f27_3_client_drop_sends_in_band_stop_and_joins_cleanly() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = test_builder(&srv, 2, 4, 4, 32).build().expect("build");

    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..4].copy_from_slice(b"stop");
    let p1 = client.store("drop-test", buf, 4);
    let p2 = client.erase("nonexistent");
    let (r1, buf) = p1.wait();
    r1.expect("p1");
    drop(buf);
    p2.wait().expect("p2");

    // Dropping the last Client handle must send in-band stop and join all I/O threads cleanly.
    drop(client);
    assert!(srv.get_object("drop-test").is_some());
}

#[test]
fn t1_f27_4_threaded_client_store_fetch_runs_under_alloc_guard() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = test_builder(&srv, 1, 4, 2, 32).build().expect("build");
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..6].copy_from_slice(b"sealed");
    // Prime connection & thread-local waker before sealing caller thread.
    let (res, buf) = client.store("warmup", buf, 6).wait();
    res.expect("warmup");

    let (ok, buf) = sealed(|| {
        let mut buf = buf;
        buf.as_mut_slice()[..6].copy_from_slice(b"sealed");
        let (r, back) = client.store("warmup", buf, 6).wait();
        if r.is_err() {
            return (false, back);
        }
        let (f, back) = client.fetch("warmup", back).wait();
        (f == Ok(Some(6)) && &back.as_slice()[..6] == b"sealed", back)
    });
    assert!(ok);
    drop(buf);
}

#[test]
fn t1_f27_5_unedited_s3_client_model_tests_pass() {
    let root = crate::e2e::harness::workspace_root();
    let model_src = std::fs::read_to_string(root.join("crates/longshore_s3/tests/client/model.rs"))
        .expect("model.rs");
    assert_eq!(
        model_src.matches("#[test]").count(),
        10,
        "expected all 10 unedited client_model tests"
    );
    let out = Command::new("cargo")
        .args([
            "test",
            "-p",
            "longshore_s3",
            "--release",
            "--target-dir",
            "target/model",
            "--test",
            "client_model",
        ])
        .env("RUSTFLAGS", "--cfg longshore_model")
        .current_dir(root)
        .output()
        .expect("run client_model");
    assert!(
        out.status.success(),
        "client_model failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(all(feature = "fault-injection", target_os = "linux"))]
#[test]
fn t2_f27_1_failed_uring_thread_answers_closed_and_keeps_lent_buffer_valid() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let mut b = test_builder(&srv, 1, 4, 2, 16);
    b.portable = false;
    let client = match b.build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..8].copy_from_slice(b"preserve");
    client.inject_fault(0, Fault::TickFails);
    for _ in 0..100 {
        if client.thread_failed(0) {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(5));
    }
    let (res, buf) = client.store("after-fail", buf, 8).wait();
    assert_eq!(res, Err(Error::Closed));
    // Unless void, lent buffer memory remains readable until Buffer/Client drop.
    if !buf.is_empty() {
        assert_eq!(&buf.as_slice()[..8], b"preserve");
    }
    drop(buf);
}

#[test]
fn t2_f27_2_dropped_pending_with_lent_buffer_returns_buffer_to_lane() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = test_builder(&srv, 1, 4, 2, 16).build().expect("build");

    let mut b0 = client.buffer_blocking();
    b0.as_mut_slice()[..5].copy_from_slice(b"first");
    let mut b1 = client.buffer_blocking();
    b1.as_mut_slice()[..6].copy_from_slice(b"second");

    // Drop Pending mid-flight carrying b0; I/O thread must free slot and reclaim b0.
    let dropped = client.store("dropped-op", b0, 5);
    drop(dropped);
    let (r1, back1) = client.store("kept-op", b1, 6).wait();
    r1.expect("kept-op");
    drop(back1);

    // Both buffers must now be borrowable again (zero leaked buffers).
    let re0 = client.buffer_blocking();
    let re1 = client.buffer_blocking();
    drop((re0, re1));
}

#[test]
fn t2_f27_3_saturate_client_beyond_operations_and_preallocated_waiters() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = Arc::new(test_builder(&srv, 1, 2, 4, 4).build().expect("build"));

    let mut handles = Vec::new();
    for i in 0..10usize {
        let c = Arc::clone(&client);
        handles.push(thread::spawn(move || {
            c.erase(&format!("missing-{i}")).wait().unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn t2_f27_4_multi_lane_routing_and_lane_isolation() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = Arc::new(test_builder(&srv, 4, 4, 2, 32).build().expect("build"));

    #[cfg(all(feature = "fault-injection", target_os = "linux"))]
    {
        let mut b = test_builder(&srv, 4, 4, 2, 32);
        b.portable = false;
        if let Ok(uring_client) = b.build() {
            uring_client.inject_fault(0, Fault::TickFails);
            for _ in 0..100 {
                if uring_client.thread_failed(0) {
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                uring_client.thread_failed(0),
                "lane 0 should fail after TickFails"
            );
            let mut healthy_ok = 0usize;
            for i in 0..8 {
                match uring_client.erase(&format!("healthy-{i}")).wait() {
                    Ok(()) => healthy_ok += 1,
                    Err(Error::Closed) => {}
                    Err(other) => panic!("unexpected error on multi-lane erase: {other:?}"),
                }
            }
            assert!(
                healthy_ok > 0,
                "healthy lanes must continue serving requests while lane 0 is closed"
            );
        }
    }

    let mut bufs = Vec::new();
    for i in 0..4usize {
        let mut b = client.buffer_blocking();
        b.as_mut_slice()[..4].copy_from_slice(b"lane");
        let (r, back) = client.store(&format!("lane-{i}"), b, 4).wait();
        r.expect("multi-lane store");
        bufs.push(back);
    }
    drop(bufs);
}

#[test]
fn t2_f27_5_store_exceeding_buffer_size_returns_too_long_without_sending() {
    let srv = HttpStoreServer::start_plain().expect("plain store");
    let client = test_builder(&srv, 1, 2, 2, 16).build().expect("build");
    let too_big = client.buffer_size() + 1;
    let buf = client.buffer_blocking();
    let (res, buf) = client.store("too-long", buf, too_big).wait();
    assert_eq!(res, Err(Error::TooLong));
    assert_eq!(srv.request_count(), 0, "must not send over mailbox/network");
    drop(buf);
}
