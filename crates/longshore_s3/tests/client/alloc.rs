// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation verification for the threaded S3 client over `longshore_mailbox`.

#![cfg(not(longshore_model))]

use longshore_common::io::alloc_guard::Guard;

#[global_allocator]
static ALLOC: Guard = Guard;

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod loopback;

#[cfg(test)]
#[path = "alloc/exact_multipart.rs"]
mod exact_multipart;

#[test]
fn builder_rejects_zero_waiters_with_setting_error() {
    use longshore_s3::client::{BuildError, Builder};
    use longshore_s3::creds::Credentials;

    let b = Builder::new(
        "bucket.s3.example.com",
        "us-east-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    )
    .nameservers(vec!["192.0.2.53:53".parse().unwrap()])
    .waiters(0);
    assert!(matches!(b.build(), Err(BuildError::Setting(_))));
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod threaded {
    use super::loopback::ServerThread;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use longshore_common::io::alloc_guard::sealed;
    use longshore_common::io::{Limits, Sock};
    use longshore_http_client::server;
    use longshore_mailbox::block_on;
    use longshore_s3::client::Client;
    use std::sync::{Arc, Barrier};

    const SIZE: u32 = 16 * 1024;
    const CONNS: u32 = 2;
    const CALLERS: usize = 4;
    const ROUNDS: usize = 6;

    #[test]
    fn threaded_client_operations_allocate_nothing_after_waker_warmup() {
        let server = ServerThread::start(
            Limits::BASE
                .with_sockets_max(1 + CONNS * 2)
                .with_buffers(2 * CONNS * 2)
                .with_buffer_size(SIZE)
                .with_ops_max(CONNS * 6 + 3),
            server::ServerConfig::builder(Sock::new(0), CONNS * 2, 1, 0, SIZE, 2 << 40)
                .head_max(4096)
                .objects_max((CALLERS * 2) as u32)
                .build()
                .unwrap(),
        );

        let b = server
            .builder(CONNS, SIZE)
            .threads(2)
            // Fewer buffers and slots than callers to force slot/buffer waiter contention.
            .buffers(1)
            .operations(1)
            .waiters(32);

        let client = b.build().expect("client build on portable backend");
        let start_barrier = Arc::new(Barrier::new(CALLERS));
        let end_barrier = Arc::new(Barrier::new(CALLERS));

        let callers: Vec<_> = (0..CALLERS)
            .map(|caller| {
                let client = client.clone();
                let start = start_barrier.clone();
                let end = end_barrier.clone();
                let key = format!("alloc-key-{caller}");
                let detach_key = format!("alloc-detach-{caller}");
                std::thread::spawn(move || {
                    // Warm this thread's block_on waker before sealing allocations.
                    block_on(core::future::ready(()));
                    let _: std::sync::BarrierWaitResult = start.wait();
                    sealed(|| caller_rounds(&client, &key, &detach_key, caller));
                    let _: std::sync::BarrierWaitResult = end.wait();
                })
            })
            .collect();

        for c in callers {
            c.join().expect("caller thread failed");
        }

        drop(client);
        server.stop();
    }

    /// One caller's `ROUNDS` rounds on its own keys, then erasing them.
    fn caller_rounds(client: &Client, key: &str, detach_key: &str, caller: usize) {
        let payload = [0x5au8; 512];
        for round in 0..ROUNDS {
            round_trip_then_drop_pending(client, key, detach_key, &payload, caller + round, round);
        }

        assert_eq!(client.erase(key).wait(), Ok(()));
        assert_eq!(block_on(client.erase(detach_key)), Ok(()));
    }

    /// Stores and fetches `payload` under `key`, alternating blocking and
    /// `block_on` forms by `buffer_turn` and `round`, then drops a pending
    /// store and a pending erase of `detach_key`.
    fn round_trip_then_drop_pending(
        client: &Client,
        key: &str,
        detach_key: &str,
        payload: &[u8],
        buffer_turn: usize,
        round: usize,
    ) {
        let mut body = if buffer_turn.is_multiple_of(2) {
            client.buffer_blocking()
        } else {
            block_on(client.buffer())
        };
        body.as_mut_slice()[..payload.len()].copy_from_slice(payload);

        let (stored, body) = if round.is_multiple_of(2) {
            client.store(key, body, payload.len()).wait()
        } else {
            block_on(client.store(key, body, payload.len()))
        };
        assert_eq!(stored, Ok(()));

        let (fetched, body) = if round.is_multiple_of(2) {
            block_on(client.fetch(key, body))
        } else {
            client.fetch(key, body).wait()
        };
        assert_eq!(fetched, Ok(Some(payload.len())));
        assert_eq!(&body.as_slice()[..payload.len()], payload);

        // Exercise dropped Pending detach with a lent buffer.
        {
            let mut cx = Context::from_waker(Waker::noop());
            let mut pending = pin!(client.store(detach_key, body, 64));
            match pending.as_mut().poll(&mut cx) {
                // Dropped in flight next: the detach is what this exercises.
                Poll::Pending => {}
                // Finished already: nothing is left to detach, which is also fine.
                Poll::Ready(_) => {}
            }
        }

        // Exercise dropped Pending detach without a lent buffer.
        {
            let mut cx = Context::from_waker(Waker::noop());
            let mut pending = pin!(client.erase(detach_key));
            match pending.as_mut().poll(&mut cx) {
                // Dropped in flight next: the detach is what this exercises.
                Poll::Pending => {}
                // Finished already: nothing is left to detach, which is also fine.
                Poll::Ready(_) => {}
            }
        }
    }
}
