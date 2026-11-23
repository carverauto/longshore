// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `alloc_guard`-sealed suites for `longshore_mailbox` queue/preallocated waiters
//! and threaded `longshore_s3::client`.

use super::super::harness::HttpStoreServer;
use longshore::client::Builder;
use longshore::io::alloc_guard::sealed;
use longshore::io::{Completions, Io, Limits, Token};
use longshore::mailbox::{mailbox, Config, TryRecv};
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

#[derive(Clone, Copy)]
struct SealedMsg {
    slot: u32,
    val: u64,
}

pub fn run_mailbox_queue_and_waiters_sealed() {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(64)
        .with_ops_max(4);
    let mut sim = SimIo::new(limits, 9003, Faults::default());
    let mut out = Completions::new(8);
    let (tx, mut io_rx) =
        mailbox::<SealedMsg, u64, _>(&mut sim, Config::new(8, 2, 8, Token::new(1)))
            .expect("mailbox");
    io_rx.arm(&mut sim).expect("arm");
    let waker = Waker::noop();

    let ok = sealed(|| {
        let mut cx = Context::from_waker(waker);
        let mut total = 0u64;
        for i in 1..=16u64 {
            let mut fut = pin!(tx.call(|slot| SealedMsg { slot, val: i }, 0));
            if !matches!(fut.as_mut().poll(&mut cx), Poll::Pending) {
                return 0;
            }
            if sim.tick(false, &mut out).is_err() {
                return 0;
            }
            while let Some(c) = out.pop() {
                if io_rx.on_completion(&mut sim, c).unwrap_or(false) {
                    while let TryRecv::Message(m) = io_rx.try_recv() {
                        let _: Option<u32> = io_rx.answer(m.slot, m.val * 2);
                    }
                }
            }
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => total += v,
                _ => return 0,
            }
        }
        total
    });
    io_rx.close(|_| {}, || 0, |_| {});
    assert_eq!(ok, (1..=16u64).map(|x| x * 2).sum::<u64>());
}

#[test]
fn sealed_mailbox_queue_and_waiters() {
    run_mailbox_queue_and_waiters_sealed();
}

pub fn run_threaded_s3_client_sealed() {
    let server = HttpStoreServer::start_plain().expect("plain store");
    let creds = Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .unwrap();
    let b = Builder::new("127.0.0.1", "us-east-1", creds)
        .port(server.port())
        .address(Some(server.addr()))
        .bucket_in_path(Some("sealed".into()))
        .tls(false)
        .portable(true)
        .threads(1)
        .buffers(2)
        .buffer_size(64 * 1024)
        .connections(2)
        .operations(4)
        .waiters(16);
    let client = b.build().expect("client");

    // Pre-borrow buffer and warm-up connection outside `sealed` so initial DNS/connect setup
    // or thread startup never occurs inside the sealed window.
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..4].copy_from_slice(b"warm");
    let (warm_res, mut buf) = client.store("warm", buf, 4).wait();
    warm_res.expect("warm store");

    let payload = *b"sealed-s3-object";
    let (st_ok, ft_len, er_ok, returned_buf) = sealed(|| {
        buf.as_mut_slice()[..payload.len()].copy_from_slice(&payload);
        let (st, buf) = client.store("sealed-obj", buf, payload.len()).wait();
        let st_ok = st.is_ok();
        let (ft, buf) = client.fetch("sealed-obj", buf).wait();
        let ft_len = ft.ok().flatten();
        let matches = ft_len == Some(payload.len()) && buf.as_slice()[..payload.len()] == payload;
        let er_ok = client.erase("sealed-obj").wait().is_ok();
        (st_ok && matches, ft_len, er_ok, buf)
    });
    drop(returned_buf);
    assert!(st_ok, "sealed store+fetch failed");
    assert_eq!(ft_len, Some(payload.len()));
    assert!(er_ok, "sealed erase failed");
}

#[test]
fn sealed_threaded_s3_client() {
    run_threaded_s3_client_sealed();
}
