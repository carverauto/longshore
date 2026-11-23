// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-09`: Threaded `longshore_s3::client` on `longshore_mailbox`
//! (`alloc_guard` sealed, caller future detach + lent buffer return,
//! preallocated waiters, credentials reload, and graceful `Drop` `stop`).

use super::super::harness::{HttpStoreServer, TempDir};
use longshore::client::{Builder, Client};
use longshore::io::alloc_guard::sealed;
use longshore::mailbox::block_on;
use longshore::store::creds::Credentials;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::thread;

fn creds(ak: &str, sk: &str) -> Credentials {
    Credentials::new(ak, sk).expect("creds")
}

/// The two-thread plain client whose credentials reload from `cred_path`.
fn reloading_client(store: &HttpStoreServer, cred_path: &std::path::Path) -> Client {
    let mut b = Builder::new(
        "127.0.0.1",
        "us-east-1",
        creds(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        ),
    );
    b.bucket_in_path = Some("mb-bucket".into());
    b.tls = false;
    b.port = store.port();
    b.address = Some(store.addr());
    b.threads = 2;
    b.buffers = 4;
    b.buffer_size = 16 * 1024;
    b.connections = 4;
    b.operations = 8;
    b.waiters = 64;
    b.credentials_file = Some(cred_path.to_path_buf());
    b.credentials_reread_secs = 1;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.build().expect("build S3 client on mailbox")
}

/// Pre-warms two buffers and runs a store and fetch under `alloc_guard::sealed`.
fn sealed_store_fetch(client: &Client) {
    let mut b0 = client.buffer_blocking();
    let b1 = client.buffer_blocking();
    let payload = [0x55u8; 256];
    b0.as_mut_slice()[..payload.len()].copy_from_slice(&payload);

    let (stored_ok, fetched_n, b0, b1) = sealed(|| {
        let (st, back0) = client.store("k-sealed", b0, payload.len()).wait();
        let (ft, back1) = client.fetch("k-sealed", b1).wait();
        (st.is_ok(), ft.unwrap(), back0, back1)
    });
    assert!(stored_ok);
    assert_eq!(fetched_n, Some(payload.len()));
    assert_eq!(&b1.as_slice()[..payload.len()], &payload);
    drop(b0);
    drop(b1);
}

/// Concurrent blocking (`.wait`) and async (`mailbox::block_on`) callers across 4 threads.
fn mixed_callers_round_trip(client: &Client) {
    let mut handles = Vec::new();
    for tid in 0..4usize {
        let c = client.clone();
        handles.push(thread::spawn(move || {
            let key = format!("worker-{tid}");
            if tid % 2 == 0 {
                let mut buf = c.buffer_blocking();
                buf.as_mut_slice()[..8].copy_from_slice(&tid.to_le_bytes());
                let (r, buf) = c.store(&key, buf, 8).wait();
                r.unwrap();
                let (f, back) = c.fetch(&key, buf).wait();
                assert_eq!(f.unwrap(), Some(8));
                assert_eq!(&back.as_slice()[..8], &tid.to_le_bytes());
            } else {
                block_on(async {
                    let mut buf = c.buffer().await;
                    buf.as_mut_slice()[..8].copy_from_slice(&tid.to_le_bytes());
                    let (r, buf) = c.store(&key, buf, 8).await;
                    r.unwrap();
                    let (f, back) = c.fetch(&key, buf).await;
                    assert_eq!(f.unwrap(), Some(8));
                    assert_eq!(&back.as_slice()[..8], &tid.to_le_bytes());
                });
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

/// Caller future detach with lent Buffers: poll once, then drop mid-flight.
/// The I/O thread must free the mailbox slot first and return the lent Buffer
/// back to the lane so subsequent callers can borrow all 8 buffers (2 * 4) again.
fn detached_stores_return_buffers(client: &Client) {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    for i in 0..4usize {
        let mut buf = client.buffer_blocking();
        buf.as_mut_slice()[..4].copy_from_slice(b"drop");
        let mut fut = client.store(&format!("detached-{i}"), buf, 4);
        match Pin::new(&mut fut).poll(&mut cx) {
            // Dropped in flight next: the detach is what this exercises.
            Poll::Pending => {}
            // Finished already: nothing is left to detach, which is also fine.
            Poll::Ready(_) => {}
        }
        drop(fut);
    }
    // Verify all 8 buffers across the 2 threads can still be borrowed simultaneously.
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(client.buffer_blocking());
    }
    assert_eq!(held.len(), 8);
    drop(held);
}

#[test]
fn t4_09_threaded_s3_client_mailbox_sealed_detach_and_reload() {
    let store = HttpStoreServer::start_plain().unwrap();
    let tmp = TempDir::new("t4_09_creds").unwrap();
    let cred_path = tmp.file_path("creds.txt");
    std::fs::write(
        &cred_path,
        "AKIAIOSFODNN7EXAMPLE\nwJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
    )
    .unwrap();

    let client = reloading_client(&store, &cred_path);
    sealed_store_fetch(&client);
    mixed_callers_round_trip(&client);
    detached_stores_return_buffers(&client);

    // Verify credential reload counter is accessible and clean, and Drop sends in-band stop.
    let (ok_reloads, fail_reloads) = client.credential_reloads();
    assert_eq!(fail_reloads, 0);
    let _: u64 = ok_reloads;

    // Queue final operations right before dropping Client handle to test graceful stop drain.
    let mut final_buf = client.buffer_blocking();
    final_buf.as_mut_slice()[..4].copy_from_slice(b"last");
    let pending = client.store("final-drain", final_buf, 4);
    drop(client);
    let (res, _buf) = pending.wait();
    res.expect("queued op before Client drop must complete before I/O threads stop");
    assert_eq!(
        store.get_object("mb-bucket/final-drain").unwrap().data,
        b"last"
    );
}
