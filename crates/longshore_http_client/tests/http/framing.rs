// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP client against the in-tree server on one simulated `Io`, with heap
//! allocation forbidden while requests run.

#[cfg(test)]
mod common;
use common::raw::raw_reply_with;
use common::*;
use longshore_http_client::head::HttpError;
use longshore_http_client::server::Behavior;

/// One GET against a hand-driven peer that answers the request with `reply`
/// and then waits for the client to close.
fn raw_reply(reply: &'static [u8]) -> Result<Response, Error> {
    raw_reply_with(client_cfg(), reply)
}

#[test]
fn a_bodyless_interim_response_is_skipped() {
    let r = raw_reply(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(r.map(|r| (r.status(), r.body_len())), Ok((200, 0)));
    let no_content = raw_reply(
        b"HTTP/1.1 100 Continue\r\nContent-Length: 0\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n",
    );
    assert_eq!(no_content.map(|reply| reply.status()), Ok(204));
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
        Faults::NONE
            .with_short_send(300_000)
            .with_partial_recv(300_000),
    );
    w.fill_tx(100, 0);
    for body in [1u32, 300, 5000, 20_000] {
        w.server.script(Behavior::StatusWithBody(412, body));
        let req = Request::new("PUT", "/k", "objects.test", 0, ADDR).with_body(Some((TX, 100)));
        w.client
            .start(&mut w.io, 500 + u64::from(body), &req)
            .unwrap();
        w.run_until_done(w.done.len() + 1);
        assert_eq!(
            w.done.last().unwrap().result().map(parts),
            Ok((412, body, 0)),
            "body of {body} bytes"
        );
    }
    ok(200, 0)(w.request("PUT", "/k", Some(100)));
    assert_eq!(
        w.client.connects(),
        1,
        "discarding error bodies must not cost the connection"
    );

    // A body as large as a whole buffer is still discarded: the limit is
    // 256 KiB. `framing_edges.rs` has the refusal just past it.
    w.server.script(Behavior::StatusWithBody(500, BUF));
    let req = Request::new("PUT", "/k", "objects.test", 0, ADDR).with_body(Some((TX, 100)));
    w.client.start(&mut w.io, 900, &req).unwrap();
    w.run_until_done(w.done.len() + 1);
    assert_eq!(
        w.done.last().unwrap().result().map(parts),
        Ok((500, BUF, 0)),
        "32 KiB is within the discard limit"
    );
}

/// Chunked responses decode to exactly the stored bytes, whatever the chunk
/// size and however the network splits chunk headers and data.
#[test]
fn chunked_responses_decode_to_the_stored_bytes() {
    let faults = Faults::NONE
        .with_short_send(300_000)
        .with_partial_recv(400_000)
        .with_delay(100_000);
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
            w.client.connects(),
            1,
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
    let small_get = Request::new("GET", "/small", "objects.test", 0, ADDR);
    w.client.start(&mut w.io, 700, &small_get).unwrap();
    w.run_until_done(w.done.len() + 1);
    let r = w
        .done
        .last()
        .unwrap()
        .result()
        .expect("chunked body without a buffer");
    assert_eq!((r.status(), r.body_len()), (200, 300));
    assert_eq!(w.client.discarded_body(&mut w.io, r.conn()), &sent[..]);
    // One that does not fit beside the head is refused.
    w.fill_tx(8_000, 1);
    ok(200, 0)(w.request("PUT", "/big", Some(8_000)));
    w.server.script(Behavior::ChunkedBody(1_000));
    let big_get = Request::new("GET", "/big", "objects.test", 0, ADDR);
    w.client.start(&mut w.io, 701, &big_get).unwrap();
    w.run_until_done(w.done.len() + 1);
    assert_eq!(w.done.last().unwrap().result(), Err(Error::BodyTooLarge));
    w.settle(30);
    assert_eq!(w.client.in_flight(), 0);
}

/// An interim (1xx) response is skipped only when it is what it claims to
/// be: bodyless and not ending the connection.
#[test]
fn interim_responses_are_skipped_only_when_bodyless() {
    let mut w = world(50, Faults::NONE.with_partial_recv(300_000));
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
    let cfg = client_builder_with(CONNS, CAP).build().unwrap();
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
    assert_eq!((w.client.idle_dirty(), w.client.connects()), (1, 2));

    // Without the check the next request does not get its own answer. On a
    // real network it would be handed the leftover bytes as its response;
    // the simulator fails the send instead, because the scripted server has
    // closed. Either way it is not the 404 the request should have had.
    let mut unchecked = world_with(
        32,
        Faults::default(),
        client_builder().check_idle(false).build().unwrap(),
    );
    unchecked.server.script(Behavior::Raw(reply));
    ok(200, body_len as u32)(unchecked.request("GET", "/k", None));
    assert_eq!(
        unchecked.request("GET", "/missing", None),
        Err(Error::Io(longshore_common::io::EPIPE))
    );
}
