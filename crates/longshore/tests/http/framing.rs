// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP client against the in-tree server on one simulated `Io`, with heap
//! allocation forbidden while requests run.

mod common;
use common::*;

/// One GET against a hand-driven peer that answers the request with `reply`
/// and then waits for the client to close.
fn raw_reply(reply: &'static [u8]) -> Result<Response, Error> {
    raw_reply_with(client_cfg(), reply)
}

fn raw_reply_with(cfg: client::Config, reply: &'static [u8]) -> Result<Response, Error> {
    const PEER: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 82), 80);
    const LISTEN: Sock = Sock(1 + CONNS * 2);
    const PEER_SOCK: Sock = Sock(2 + CONNS * 2);
    const ACCEPT: u64 = 3 << 40;
    const RECV: u64 = ACCEPT + 1;
    const SEND: u64 = ACCEPT + 2;
    const OUT_OFF: u32 = 4096;
    let mut w = world_with(12, Faults::default(), cfg);
    w.io.listen(LISTEN, PEER).unwrap();
    w.io.submit(
        Token(ACCEPT),
        Op::Accept {
            listener: LISTEN,
            sock: PEER_SOCK,
        },
    )
    .unwrap();
    w.io.buf(TX)[OUT_OFF as usize..OUT_OFF as usize + reply.len()].copy_from_slice(reply);
    let req = Request {
        method: "GET",
        target: "/raw",
        host: "raw.test",
        host_id: 2,
        addr: PEER,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    w.client.start(&mut w.io, 9, &req).unwrap();
    let (mut sent, mut result) = (false, None);
    let World {
        io,
        client,
        server,
        out,
        ..
    } = &mut w;
    sealed(|| {
        while result.is_none() {
            io.tick(true, out).expect("simulation deadlock");
            while let Some(c) = out.pop() {
                match c.token.0 {
                    HEARTBEAT => {
                        client.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                    }
                    ACCEPT => io
                        .submit(
                            Token(RECV),
                            Op::Recv {
                                sock: PEER_SOCK,
                                buf: TX,
                                off: 0,
                                len: 1024,
                            },
                        )
                        .unwrap(),
                    RECV if c.result > 0 => {
                        if !sent {
                            io.submit(
                                Token(SEND),
                                Op::Send {
                                    sock: PEER_SOCK,
                                    buf: TX,
                                    off: OUT_OFF,
                                    len: reply.len() as u32,
                                    zero_copy: false,
                                },
                            )
                            .unwrap();
                            sent = true;
                        }
                        io.submit(
                            Token(RECV),
                            Op::Recv {
                                sock: PEER_SOCK,
                                buf: TX,
                                off: 0,
                                len: 1024,
                            },
                        )
                        .unwrap();
                    }
                    RECV | SEND => {}
                    _ => match client.on_completion(io, c) {
                        Handled::Done(d) => result = Some(d.result),
                        Handled::Pending => {}
                        Handled::NotMine => {
                            assert!(server.on_completion(io, c), "unrouted completion {c:?}")
                        }
                    },
                }
            }
        }
    });
    result.unwrap()
}

#[test]
fn a_bodyless_interim_response_is_skipped() {
    let r = raw_reply(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(r.map(|r| (r.status, r.body_len)), Ok((200, 0)));
    let r = raw_reply(
        b"HTTP/1.1 100 Continue\r\nContent-Length: 0\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n",
    );
    assert_eq!(r.map(|r| r.status), Ok(204));
}

#[test]
fn an_interim_response_that_declares_a_body_or_close_is_rejected() {
    // RFC 9110 15.2: a 1xx response is terminated by its header section and has no content.
    let forged = raw_reply(b"HTTP/1.1 100 Continue\r\nContent-Length: 39\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(
        forged,
        Err(Error::Head(HttpError::BadFraming)),
        "the 100's body must not be read as the final response"
    );
    let chunked = raw_reply(b"HTTP/1.1 100 Continue\r\nTransfer-Encoding: chunked\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(chunked, Err(Error::Head(HttpError::BadFraming)));
    let close = raw_reply(b"HTTP/1.1 100 Continue\r\nConnection: close\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(
        close,
        Err(Error::Head(HttpError::BadFraming)),
        "a close on the interim head must not be forgotten"
    );
}

/// Real stores send an error document with most non-2xx statuses, even when
/// the caller expects no body. The client must read it and keep the connection.
#[test]
fn an_unexpected_response_body_is_discarded_and_the_connection_survives() {
    let mut w = world(
        11,
        Faults {
            short_send: 300_000,
            partial_recv: 300_000,
            ..Faults::default()
        },
    );
    w.fill_tx(100, 0);
    for body in [1u32, 300, 5000, 20_000] {
        w.server.script(Behavior::StatusWithBody(412, body));
        let req = Request {
            method: "PUT",
            target: "/k",
            host: "objects.test",
            host_id: 0,
            addr: ADDR,
            headers: &[],
            body: Some((TX, 100)),
            response_buf: None,
        };
        w.client.start(&mut w.io, 500 + body as u64, &req).unwrap();
        w.run_until_done(w.done.len() + 1);
        assert_eq!(
            w.done.last().unwrap().result,
            Ok(Response {
                status: 412,
                body_len: body,
                conn: 0
            }),
            "body of {body} bytes"
        );
    }
    ok(200, 0)(w.request("PUT", "/k", Some(100)));
    assert_eq!(
        w.client.connects, 1,
        "discarding error bodies must not cost the connection"
    );

    // Too large to discard: refused rather than read without bound.
    w.server.script(Behavior::StatusWithBody(500, BUF));
    let req = Request {
        method: "PUT",
        target: "/k",
        host: "objects.test",
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: Some((TX, 100)),
        response_buf: None,
    };
    w.client.start(&mut w.io, 900, &req).unwrap();
    w.run_until_done(w.done.len() + 1);
    assert_eq!(
        w.done.last().unwrap().result,
        Ok(Response {
            status: 500,
            body_len: BUF,
            conn: 0
        }),
        "32 KiB is within the discard limit"
    );
}

/// Chunked responses decode to exactly the stored bytes, whatever the chunk
/// size and however the network splits chunk headers and data.
#[test]
fn chunked_responses_decode_to_the_stored_bytes() {
    let faults = Faults {
        short_send: 300_000,
        partial_recv: 400_000,
        delay: 100_000,
        ..Faults::default()
    };
    for seed in 0..12u64 {
        let mut w = world(seed, faults);
        // Leave room in the server's buffer for the chunk framing.
        let len = 20_000u32;
        w.fill_tx(len, seed as u8);
        ok(200, 0)(w.request("PUT", "/k", Some(len)));
        let sent = w.io.buf(TX)[..len as usize].to_vec();
        for chunk in [1_000u32, 4_096, 7, 20_000, 65_536] {
            // Tiny chunks would not fit the server's buffer for the whole object.
            if chunk < 100 && len / chunk > 2_000 {
                continue;
            }
            w.server.script(Behavior::ChunkedBody(chunk));
            ok(200, len)(w.request("GET", "/k", None));
            assert!(
                w.io.buf(RX)[..len as usize] == sent[..],
                "seed {seed}, chunk size {chunk}: decoded bytes differ"
            );
        }
        assert_eq!(
            w.client.connects, 1,
            "chunked responses keep the connection usable"
        );
    }
}

#[test]
fn chunked_edge_cases() {
    let mut w = world(40, Faults::default());
    // An empty chunked body.
    w.server.script(Behavior::Chunked);
    ok(200, 0)(w.request("GET", "/k", None));
    // A chunked body the caller gave no buffer for is kept beside the head if it fits.
    w.fill_tx(300, 9);
    ok(200, 0)(w.request("PUT", "/small", Some(300)));
    let sent = w.io.buf(TX)[..300].to_vec();
    w.server.script(Behavior::ChunkedBody(64));
    let req = Request {
        method: "GET",
        target: "/small",
        host: "objects.test",
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: None,
    };
    w.client.start(&mut w.io, 700, &req).unwrap();
    w.run_until_done(w.done.len() + 1);
    let r = w
        .done
        .last()
        .unwrap()
        .result
        .expect("chunked body without a buffer");
    assert_eq!((r.status, r.body_len), (200, 300));
    assert_eq!(w.client.discarded_body(&mut w.io, r.conn), &sent[..]);
    // One that does not fit beside the head is refused.
    w.fill_tx(8_000, 1);
    ok(200, 0)(w.request("PUT", "/big", Some(8_000)));
    w.server.script(Behavior::ChunkedBody(1_000));
    let req = Request {
        method: "GET",
        target: "/big",
        host: "objects.test",
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: None,
    };
    w.client.start(&mut w.io, 701, &req).unwrap();
    w.run_until_done(w.done.len() + 1);
    assert_eq!(w.done.last().unwrap().result, Err(Error::BodyTooLarge));
    w.settle(30);
    assert_eq!(w.client.in_flight(), 0);
}

/// An interim (1xx) response is skipped only when it is what it claims to
/// be: bodyless and not ending the connection.
#[test]
fn interim_responses_are_skipped_only_when_bodyless() {
    let mut w = world(
        50,
        Faults {
            partial_recv: 300_000,
            ..Faults::default()
        },
    );
    w.server.script(Behavior::Raw(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
    ));
    ok(204, 0)(w.request("GET", "/k", None));
    w.server.script(Behavior::Raw(b"HTTP/1.1 103 Early Hints\r\nContent-Length: 0\r\nLink: </x>\r\n\r\nHTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"));
    ok(200, 2)(w.request("GET", "/k", None));

    // A 1xx that declares a body: its "body" here is a forged 200.
    let forged: [&'static [u8]; 3] = [
        b"HTTP/1.1 100 Continue\r\nContent-Length: 39\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\nHTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 100 Continue\r\nTransfer-Encoding: chunked\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 100 Continue\r\nConnection: close\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
    ];
    for raw in forged {
        w.settle(30);
        w.server.script(Behavior::Raw(raw));
        assert_eq!(
            w.request("GET", "/k", None),
            Err(Error::Head(HttpError::BadFraming)),
            "{:?}",
            String::from_utf8_lossy(raw)
        );
    }
    w.settle(30);
    assert_eq!(w.client.in_flight(), 0);
}

/// 101 is a final answer that hands the connection to another protocol.
/// Skipping it like 100 would read that protocol's bytes as the response.
#[test]
fn switching_protocols_is_not_skipped_as_an_interim_response() {
    let reply = b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: other\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(raw_reply(reply), Err(Error::Head(HttpError::BadFraming)));
    // Other interim statuses are still skipped.
    ok(200, 0)(raw_reply(b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"));
}

/// A chunked body exactly as large as the buffer is allowed, like a body of
/// that size with a Content-Length. One byte more is too large.
#[test]
fn a_chunked_body_that_exactly_fills_the_buffer_is_accepted() {
    const CAP: u32 = 8192;
    let cfg = client::Config {
        buffer_size: CAP,
        ..client_cfg()
    };
    let chunked = |len: usize| -> &'static [u8] {
        let mut reply = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let half = len / 2;
        for part in [half, len - half] {
            reply.extend(format!("{part:x}\r\n").into_bytes());
            reply.extend(std::iter::repeat_n(b'c', part));
            reply.extend(b"\r\n");
        }
        reply.extend(b"0\r\n\r\n");
        Box::leak(reply.into_boxed_slice())
    };
    ok(200, CAP - 1)(raw_reply_with(cfg, chunked(CAP as usize - 1)));
    ok(200, CAP)(raw_reply_with(cfg, chunked(CAP as usize)));
    assert_eq!(
        raw_reply_with(cfg, chunked(CAP as usize + 1)),
        Err(Error::BodyTooLarge)
    );
}

/// Bytes sent after a complete response, arriving in a later read than the
/// response's last byte. They belong to nothing, and must not be read as the
/// answer to the next request on the connection.
#[test]
fn bytes_after_a_response_in_a_later_read_are_not_taken_for_the_next_response() {
    // The body is longer than the head buffer, so the client's last read
    // asks for exactly the rest of the body and the extra bytes stay behind.
    let body_len = HEAD_MAX as usize + 900;
    let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {body_len}\r\n\r\n").into_bytes();
    reply.extend(std::iter::repeat_n(b'b', body_len));
    reply.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nwrong");
    let reply: &'static [u8] = Box::leak(reply.into_boxed_slice());

    let mut w = world(32, Faults::default());
    w.server.script(Behavior::Raw(reply));
    ok(200, body_len as u32)(w.request("GET", "/k", None));
    // The next request must get its own answer, from a new connection.
    ok(404, 0)(w.request("GET", "/missing", None));
    assert_eq!((w.client.idle_dirty, w.client.connects), (1, 2));

    // Without the check the next request does not get its own answer. On a
    // real network it would be handed the leftover bytes as its response;
    // the simulator fails the send instead, because the scripted server has
    // closed. Either way it is not the 404 the request should have had.
    let mut w = world_with(
        32,
        Faults::default(),
        client::Config {
            check_idle: false,
            ..client_cfg()
        },
    );
    w.server.script(Behavior::Raw(reply));
    ok(200, body_len as u32)(w.request("GET", "/k", None));
    assert_ne!(
        w.request("GET", "/missing", None).map(|r| r.status),
        Ok(404)
    );
}
