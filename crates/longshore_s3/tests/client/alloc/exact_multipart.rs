// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation verification for `Client::put`/`head`/`get` and multipart
//! (`upload_begin`/`upload_part`/`upload_complete`/`upload_abort`) under `sealed`.

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn threaded_client_exact_and_multipart_allocate_nothing_under_sealed() {
    use longshore_common::io::alloc_guard::sealed;
    use longshore_common::io::posix::PosixIo;
    use longshore_common::io::{Completions, Io, Limits, Op, Sock, Token};
    use longshore_http_client::server::{self, Server};
    use longshore_mailbox::block_on;
    use longshore_s3::client::{Budget, Builder};
    use longshore_s3::creds::Credentials;
    use longshore_s3::report::{End, Expect, Put};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const SIZE: u32 = 16 * 1024;
    const CONNS: u32 = 2;
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
            objects_max: 8,
            token_base: 2 << 40,
            checksum: Some(|body: &[u8]| {
                longshore_crc::crc64_header_value(longshore_crc::crc64_nvme(body))
            }),
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
                    server.on_completion(&mut io, c);
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
    b.threads = 1;
    b.connections = CONNS;
    b.buffers = 2;
    b.buffer_size = SIZE;
    b.operations = 4;
    b.waiters = 8;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }

    let client = b.build().expect("client build");
    block_on(core::future::ready(()));
    let budget = Budget::once(Instant::now() + Duration::from_secs(60));
    let payload = [0x3Cu8; 256];

    sealed(|| {
        let mut buf = client.buffer_blocking();
        buf.as_mut_slice()[..payload.len()].copy_from_slice(&payload);
        let (put_res, buf) = client
            .put("exact-k", buf, payload.len(), &Put::default(), budget)
            .wait();
        let rep = put_res.unwrap();
        assert!(matches!(rep.end, End::Answered(a) if a.status == 200));

        let head_res = block_on(client.head("exact-k", &Expect::default(), budget)).unwrap();
        assert!(matches!(head_res.end, End::Answered(a) if a.status == 200));

        let (get_res, mut buf) = client
            .get("exact-k", None, &Expect::default(), buf, budget)
            .wait();
        let gre = get_res.unwrap();
        assert!(matches!(gre.end, End::Answered(a) if a.status == 200));
        assert_eq!(&buf.as_slice()[..payload.len()], &payload);

        // Multipart complete + abort under sealed.
        let up = client.upload_begin("mp-k").wait().unwrap();
        buf.as_mut_slice()[..8].copy_from_slice(b"part-one");
        let (p1, buf) = client.upload_part("mp-k", &up, 1, buf, 8).wait();
        let p1 = p1.unwrap();
        let (comp, mut buf) = client.upload_complete("mp-k", &up, &[p1], buf).wait();
        comp.unwrap();

        let up2 = block_on(client.upload_begin("mp-abort")).unwrap();
        buf.as_mut_slice()[..4].copy_from_slice(b"part");
        let (p2, buf) = block_on(client.upload_part("mp-abort", &up2, 1, buf, 4));
        p2.unwrap();
        block_on(client.upload_abort("mp-abort", &up2)).unwrap();
        drop(buf);
    });

    drop(client);
    stop.store(true, Ordering::Release);
    server_wake.wake();
    server_thread.join().expect("server thread");
}
