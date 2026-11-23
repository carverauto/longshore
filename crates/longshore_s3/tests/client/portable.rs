// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Client integration tests on the portable POSIX backend (`kqueue` on macOS,
//! `epoll` on Linux).

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod loopback;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn client_stores_and_fetches_over_loopback_on_portable_backend() {
    use longshore_common::io::{Limits, Sock};
    use longshore_http_client::server;
    use loopback::ServerThread;

    const SIZE: u32 = 64 * 1024;
    const CONNS: u32 = 2;

    let server = ServerThread::start(
        Limits::BASE
            .with_sockets_max(1 + CONNS)
            .with_buffers(2 * CONNS)
            .with_buffer_size(SIZE)
            .with_ops_max(CONNS * 3 + 3),
        server::ServerConfig::builder(Sock::new(0), CONNS, 1, 0, SIZE, 2 << 40)
            .head_max(4096)
            .objects_max(4)
            .build()
            .unwrap(),
    );

    let b = server.builder(CONNS, SIZE).buffers(2).operations(4);

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
    server.stop();
}

#[cfg(target_os = "macos")]
#[test]
fn pinning_threads_to_cores_is_refused_on_macos() {
    use longshore_s3::client::{BuildError, Builder};
    use longshore_s3::creds::Credentials;

    let b = Builder::new(
        "bucket.s3.example.com",
        "us-east-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    )
    .nameservers(vec!["192.0.2.53:53".parse().unwrap()])
    .pin(Some((0, 1)));
    assert!(matches!(b.build(), Err(BuildError::Setting(_))));
}
