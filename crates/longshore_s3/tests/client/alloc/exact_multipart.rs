// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation verification for `Client::put`/`head`/`get` and multipart
//! (`upload_begin`/`upload_part`/`upload_complete`/`upload_abort`) under `sealed`.

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn threaded_client_exact_and_multipart_allocate_nothing_under_sealed() {
    use super::loopback::ServerThread;
    use longshore_common::io::alloc_guard::sealed;
    use longshore_common::io::{Limits, Sock};
    use longshore_http_client::server;
    use longshore_mailbox::block_on;
    use longshore_s3::client::Budget;
    use std::time::{Duration, Instant};

    const SIZE: u32 = 16 * 1024;
    const CONNS: u32 = 2;

    let server = ServerThread::start(
        Limits::BASE
            .with_sockets_max(1 + CONNS * 2)
            .with_buffers(2 * CONNS * 2)
            .with_buffer_size(SIZE)
            .with_ops_max(CONNS * 6 + 3),
        server::ServerConfig::builder(Sock::new(0), CONNS * 2, 1, 0, SIZE, 2 << 40)
            .head_max(4096)
            .objects_max(8)
            .checksum(|body: &[u8]| {
                longshore_crc::crc64_header_value(longshore_crc::crc64_nvme(body))
            })
            .build()
            .unwrap(),
    );

    let b = server
        .builder(CONNS, SIZE)
        .threads(1)
        .buffers(2)
        .operations(4)
        .waiters(8);

    let client = b.build().expect("client build");
    block_on(core::future::ready(()));
    let budget = Budget::once(Instant::now() + Duration::from_secs(60));

    sealed(|| {
        let buf = put_head_get_exact(&client, budget);
        complete_then_abort_multipart(&client, buf);
    });

    drop(client);
    server.stop();
}

/// Put, head and get one object; returns the buffer for reuse.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn put_head_get_exact(
    client: &longshore_s3::client::Client,
    budget: longshore_s3::client::Budget,
) -> longshore_s3::client::Buffer {
    use longshore_mailbox::block_on;
    use longshore_s3::report::{End, Expect, Put};

    let payload = [0x3Cu8; 256];
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..payload.len()].copy_from_slice(&payload);
    let (put_res, buf) = client
        .put("exact-k", buf, payload.len(), &Put::default(), budget)
        .wait();
    let rep = put_res.unwrap();
    assert!(matches!(rep.end, End::Answered(a) if a.status == 200));

    let head_res = block_on(client.head("exact-k", &Expect::default(), budget)).unwrap();
    assert!(matches!(head_res.end, End::Answered(a) if a.status == 200));

    let (get_res, buf) = client
        .get("exact-k", None, &Expect::default(), buf, budget)
        .wait();
    let gre = get_res.unwrap();
    assert!(matches!(gre.end, End::Answered(a) if a.status == 200));
    assert_eq!(&buf.as_slice()[..payload.len()], &payload);
    buf
}

/// Multipart complete + abort under sealed.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn complete_then_abort_multipart(
    client: &longshore_s3::client::Client,
    mut buf: longshore_s3::client::Buffer,
) {
    use longshore_mailbox::block_on;

    let up = client.upload_begin("mp-k").wait().unwrap();
    buf.as_mut_slice()[..8].copy_from_slice(b"part-one");
    let (p1, buf) = client.upload_part("mp-k", &up, 1, buf, 8).wait();
    let p1 = p1.unwrap();
    let (comp, mut buf) = client.upload_complete("mp-k", &up, &[p1], buf).wait();
    comp.unwrap();

    let up2 = block_on(client.upload_begin("mp-abort")).unwrap();
    buf.as_mut_slice()[..4].copy_from_slice(b"part");
    let (p2, buf) = block_on(client.upload_part("mp-abort", &up2, 1, buf, 4));
    let _: longshore_s3::multipart::Part = p2.unwrap();
    block_on(client.upload_abort("mp-abort", &up2)).unwrap();
    drop(buf);
}
