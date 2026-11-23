// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The in-tree server's own refusals, driven by hand over a raw socket.
//! Every other crate's tests lean on this server, and the client never
//! sends it a malformed request, so nothing else reaches these answers.

mod common;
use common::*;

/// Send `request` on a new connection and return the head of the answer.
fn answer_to(request: &[u8]) -> String {
    const SOCK: Sock = Sock(1 + CONNS * 2);
    const CONNECT: u64 = 3 << 40;
    const SEND: u64 = CONNECT + 1;
    const RECV: u64 = CONNECT + 2;
    let mut w = world(50, Faults::default());
    w.io.buf(TX)[..request.len()].copy_from_slice(request);
    w.io.submit(
        Token(CONNECT),
        Op::Connect {
            sock: SOCK,
            addr: ADDR,
        },
    )
    .unwrap();
    let mut got = 0u32;
    let World {
        io, server, out, ..
    } = &mut w;
    let complete = |sim: &mut SimIo, len: u32| {
        sim.buf(RX)[..len as usize]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
    };
    sealed(|| {
        while !complete(io, got) {
            io.tick(true, out).expect("simulation deadlock");
            while let Some(c) = out.pop() {
                match c.token.0 {
                    HEARTBEAT => io
                        .submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                        .unwrap(),
                    CONNECT => {
                        assert_eq!(c.result, 0, "connect");
                        io.submit(
                            Token(SEND),
                            Op::Send {
                                sock: SOCK,
                                buf: TX,
                                off: 0,
                                len: request.len() as u32,
                                zero_copy: false,
                            },
                        )
                        .unwrap();
                        io.submit(
                            Token(RECV),
                            Op::Recv {
                                sock: SOCK,
                                buf: RX,
                                off: 0,
                                len: 1024,
                            },
                        )
                        .unwrap();
                    }
                    SEND => {}
                    RECV => {
                        assert!(c.result > 0, "closed before answering: {}", c.result);
                        got += u32::try_from(c.result).unwrap();
                        if !complete(io, got) {
                            io.submit(
                                Token(RECV),
                                Op::Recv {
                                    sock: SOCK,
                                    buf: RX,
                                    off: got,
                                    len: 1024,
                                },
                            )
                            .unwrap();
                        }
                    }
                    _ => assert!(server.on_completion(io, c), "unrouted completion {c:?}"),
                }
            }
        }
    });
    let head = &w.io.buf(RX)[..got as usize];
    let end = head
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    String::from_utf8_lossy(&head[..end]).into_owned()
}

fn status_of(request: &[u8]) -> String {
    let head = answer_to(request);
    head.lines().next().unwrap_or("").to_string()
}

#[test]
fn a_well_formed_request_is_answered_on_its_merits() {
    assert_eq!(
        status_of(b"GET /missing HTTP/1.1\r\nHost: objects.test\r\n\r\n"),
        "HTTP/1.1 404 Not Found"
    );
    let head = answer_to(b"PUT /k HTTP/1.1\r\nHost: objects.test\r\nContent-Length: 2\r\n\r\nok");
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
}

/// Each of these is refused with 400 and the connection is closed: the
/// server cannot tell where the next request would start.
#[test]
fn requests_it_cannot_frame_are_refused_and_the_connection_closed() {
    let long_target = format!("GET /{} HTTP/1.1\r\n\r\n", "t".repeat(300));
    let too_large = format!("PUT /k HTTP/1.1\r\nContent-Length: {}\r\n\r\n", BUF + 1);
    for (why, request) in [
        (
            "not HTTP",
            &b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03\r\n\r\n"[..],
        ),
        ("no version", b"GET /k\r\n\r\n"),
        ("HTTP/2", b"GET /k HTTP/2\r\n\r\n"),
        (
            "a header without a colon",
            b"GET /k HTTP/1.1\r\nNoColon\r\n\r\n",
        ),
        (
            "a chunked body",
            b"PUT /k HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        ),
        (
            "two framings",
            b"PUT /k HTTP/1.1\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n",
        ),
        (
            "two lengths",
            b"PUT /k HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nxx",
        ),
        ("a body larger than its buffer", too_large.as_bytes()),
        ("a target longer than it stores", long_target.as_bytes()),
        (
            "bytes beyond the declared body",
            b"PUT /k HTTP/1.1\r\nContent-Length: 1\r\n\r\nxy",
        ),
        (
            "a second request in the same bytes as a bodyless one",
            b"GET /k HTTP/1.1\r\n\r\nGET /k HTTP/1.1\r\n\r\n",
        ),
    ] {
        let head = answer_to(request);
        assert!(
            head.starts_with("HTTP/1.1 400 Bad Request"),
            "{why}: {head}"
        );
        assert!(
            head.to_ascii_lowercase().contains("connection: close"),
            "{why}: the answer does not announce the close: {head}"
        );
    }
}
