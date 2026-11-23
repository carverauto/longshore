// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Client integration tests on the portable POSIX backend (`kqueue` on macOS,
//! `epoll` on Linux).

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn client_stores_and_fetches_over_loopback_on_portable_backend() {
    use longshore_common::io::posix::PosixIo;
    use longshore_common::io::{Completions, Io, Limits, Op, Sock, Token};
    use longshore_http_client::server::{self, Server};
    use longshore_s3::client::Builder;
    use longshore_s3::creds::Credentials;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;

    const SIZE: u32 = 64 * 1024;
    const CONNS: u32 = 2;
    const WAKE_TOKEN: Token = Token(9 << 40);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = stop.clone();
    let (ready_tx, ready_rx) = channel();

    let server_thread = std::thread::spawn(move || {
        let mut io = PosixIo::new(Limits {
            sockets_max: 1 + CONNS,
            buffers: 2 * CONNS,
            buffer_size: SIZE,
            ops_max: CONNS * 3 + 3,
            ..Limits::BASE
        })
        .expect("server PosixIo");
        let waker = io.waker().expect("server waker");
        let mut server = Server::new(server::Config {
            listener: Sock(0),
            connections_max: CONNS,
            first_sock: 1,
            first_buf: 0,
            head_max: 4096,
            buffer_size: SIZE,
            objects_max: 4,
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
    b.connections = CONNS;
    b.buffers = 2;
    b.buffer_size = SIZE;
    b.operations = 4;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }

    let client = b.build().expect("client build on portable backend");

    let payload: Vec<u8> = (0..16_384u32)
        .map(|i| ((i * 31 + 7) & 0xff) as u8)
        .collect();

    let mut body = client.buffer_blocking();
    body.as_mut_slice()[..payload.len()].copy_from_slice(&payload);
    let (stored, body) = client.store("portable-obj", body, payload.len()).wait();
    assert_eq!(stored, Ok(()), "store failed");

    let (fetched, body) = client.fetch("portable-obj", body).wait();
    assert_eq!(fetched, Ok(Some(payload.len())), "fetch failed");
    assert_eq!(&body.as_slice()[..payload.len()], payload.as_slice());

    drop(body);
    assert_eq!(client.erase("portable-obj").wait(), Ok(()));
    let (gone, _body) = client
        .fetch("portable-obj", client.buffer_blocking())
        .wait();
    assert_eq!(gone, Ok(None));

    drop(client);
    stop.store(true, Ordering::Release);
    server_wake.wake();
    server_thread.join().expect("server thread");
}

#[cfg(target_os = "macos")]
#[test]
fn pinning_threads_to_cores_is_refused_on_macos() {
    use longshore_s3::client::{BuildError, Builder};
    use longshore_s3::creds::Credentials;

    let mut b = Builder::new(
        "bucket.s3.example.com",
        "us-east-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    );
    b.nameservers = vec!["192.0.2.53:53".parse().unwrap()];
    b.pin = Some((0, 1));
    assert!(matches!(b.build(), Err(BuildError::Setting(_))));
}
