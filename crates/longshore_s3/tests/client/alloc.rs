// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation verification for the threaded S3 client over `longshore_mailbox`.

#![cfg(not(longshore_model))]

use longshore_common::io::alloc_guard::Guard;

#[global_allocator]
static ALLOC: Guard = Guard;

#[path = "alloc/exact_multipart.rs"]
mod exact_multipart;

#[test]
fn builder_rejects_zero_waiters_with_setting_error() {
    use longshore_s3::client::{BuildError, Builder};
    use longshore_s3::creds::Credentials;

    let mut b = Builder::new(
        "bucket.s3.example.com",
        "us-east-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    );
    b.nameservers = vec!["192.0.2.53:53".parse().unwrap()];
    b.waiters = 0;
    assert!(matches!(b.build(), Err(BuildError::Setting(_))));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn threaded_client_operations_allocate_nothing_after_waker_warmup() {
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use longshore_common::io::alloc_guard::sealed;
    use longshore_common::io::posix::PosixIo;
    use longshore_common::io::{Completions, Io, Limits, Op, Sock, Token};
    use longshore_http_client::server::{self, Server};
    use longshore_mailbox::block_on;
    use longshore_s3::client::Builder;
    use longshore_s3::creds::Credentials;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Barrier};

    const SIZE: u32 = 16 * 1024;
    const CONNS: u32 = 2;
    const CALLERS: usize = 4;
    const ROUNDS: usize = 6;
    const WAKE_TOKEN: Token = Token(9 << 40);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = stop.clone();
    let (ready_tx, ready_rx) = channel();

    let server_thread = std::thread::spawn(move || {
        let mut io = PosixIo::new(Limits {
            sockets_max: 1 + CONNS * 2,
            buffers: 2 * CONNS * 2,
            buffer_size: SIZE,
            ops_max: CONNS * 6 + 3,
            ..Limits::BASE
        })
        .expect("server PosixIo");
        let waker = io.waker().expect("server waker");
        let mut server = Server::new(server::Config {
            listener: Sock(0),
            connections_max: CONNS * 2,
            first_sock: 1,
            first_buf: 0,
            head_max: 4096,
            buffer_size: SIZE,
            objects_max: (CALLERS * 2) as u32,
            token_base: 2 << 40,
            checksum: None,
        });
        let addr = server
            .start(&mut io, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("server start");
        io.submit(WAKE_TOKEN, Op::AwaitWake).expect("arm wake");
        ready_tx.send((addr, waker)).unwrap();

        let mut out = Completions::new(32);
        while !stop_server.load(Ordering::Acquire) {
            io.tick(true, &mut out).expect("server tick");
            while let Some(c) = out.pop() {
                if c.token == WAKE_TOKEN {
                    if !stop_server.load(Ordering::Acquire) {
                        io.submit(WAKE_TOKEN, Op::AwaitWake).expect("rearm wake");
                    }
                } else {
                    let _: bool = server.on_completion(&mut io, c);
                }
            }
        }
    });

    let (addr, server_wake) = ready_rx.recv().expect("server did not start");

    let mut b = Builder::new(
        "bucket.objects.test",
        "us-test-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    );
    b.port = addr.port();
    b.address = Some(addr);
    b.threads = 2;
    b.connections = CONNS;
    // Fewer buffers and slots than callers to force slot/buffer waiter contention.
    b.buffers = 1;
    b.buffer_size = SIZE;
    b.operations = 1;
    b.waiters = 32;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }

    let client = b.build().expect("client build on portable backend");
    let start_barrier = Arc::new(Barrier::new(CALLERS));
    let end_barrier = Arc::new(Barrier::new(CALLERS));
    let payload = [0x5au8; 512];

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

                sealed(|| {
                    for round in 0..ROUNDS {
                        let mut body = if (caller + round) % 2 == 0 {
                            client.buffer_blocking()
                        } else {
                            block_on(client.buffer())
                        };
                        body.as_mut_slice()[..payload.len()].copy_from_slice(&payload);

                        let (stored, body) = if round % 2 == 0 {
                            client.store(&key, body, payload.len()).wait()
                        } else {
                            block_on(client.store(&key, body, payload.len()))
                        };
                        assert_eq!(stored, Ok(()));

                        let (fetched, body) = if round % 2 == 0 {
                            block_on(client.fetch(&key, body))
                        } else {
                            client.fetch(&key, body).wait()
                        };
                        assert_eq!(fetched, Ok(Some(payload.len())));
                        assert_eq!(&body.as_slice()[..payload.len()], &payload);

                        // Exercise dropped Pending detach with a lent buffer.
                        {
                            let mut cx = Context::from_waker(Waker::noop());
                            let mut pending = pin!(client.store(&detach_key, body, 64));
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
                            let mut pending = pin!(client.erase(&detach_key));
                            match pending.as_mut().poll(&mut cx) {
                                // Dropped in flight next: the detach is what this exercises.
                                Poll::Pending => {}
                                // Finished already: nothing is left to detach, which is also fine.
                                Poll::Ready(_) => {}
                            }
                        }
                    }

                    assert_eq!(client.erase(&key).wait(), Ok(()));
                    assert_eq!(block_on(client.erase(&detach_key)), Ok(()));
                });

                let _: std::sync::BarrierWaitResult = end.wait();
            })
        })
        .collect();

    for c in callers {
        c.join().expect("caller thread failed");
    }

    drop(client);
    stop.store(true, Ordering::Release);
    server_wake.wake();
    server_thread.join().expect("server thread");
}
