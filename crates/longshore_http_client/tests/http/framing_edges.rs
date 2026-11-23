// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Framing at its edges, against a hand-driven peer: bytes after a complete
//! response, responses the client must refuse, timeouts in every state that
//! waits for the peer, and the request bytes themselves.

mod common;
use common::*;
use longshore_http_client::head::HttpError;
use longshore_http_client::server::Behavior;

fn get(reply: &[u8]) -> Result<Response, Error> {
    raw_exchange(client_cfg(), "GET", &[], Some(RX), reply).result
}

/// A GET whose response body the caller gave no buffer for.
fn get_discarding(reply: &[u8]) -> Result<Response, Error> {
    raw_exchange(client_cfg(), "GET", &[], None, reply).result
}

const BAD_FRAMING: Result<Response, Error> = Err(Error::Head(HttpError::BadFraming));

/// Bytes after a complete response, arriving in the same read as its last
/// byte, would be read as the answer to the next request. Each form of
/// response has its own check.
#[test]
fn bytes_after_a_complete_response_in_the_same_read_are_refused() {
    const NEXT: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    for head in [
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 204 No Content\r\n\r\n",
        "HTTP/1.1 304 Not Modified\r\nContent-Length: 9\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nTrailer: x\r\n\r\n",
    ] {
        // The response alone is fine.
        assert!(get(head.as_bytes()).is_ok(), "{head:?}");
        for extra in [NEXT, "X"] {
            let reply = format!("{head}{extra}");
            assert_eq!(get(reply.as_bytes()), BAD_FRAMING, "{reply:?}");
            assert_eq!(get_discarding(reply.as_bytes()), BAD_FRAMING, "{reply:?}");
        }
    }
}

/// The same when a chunked body exactly fills the buffer and its last chunk
/// is read in a second step.
#[test]
fn bytes_after_a_chunked_body_that_fills_the_buffer_are_refused() {
    const CAP: usize = 8192;
    let cfg = client::Config {
        buffer_size: CAP as u32,
        ..client_cfg()
    };
    let reply = |tail: &str| {
        let mut r = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        r.extend(format!("{CAP:x}\r\n").into_bytes());
        r.extend(std::iter::repeat_n(b'c', CAP));
        r.extend(tail.as_bytes());
        r
    };
    let run = |tail: &str| raw_exchange(cfg, "GET", &[], Some(RX), &reply(tail)).result;
    ok(200, CAP as u32)(run("\r\n0\r\n\r\n"));
    ok(200, CAP as u32)(run("\r\n0\r\nTrailer: x\r\n\r\n"));
    assert_eq!(run("\r\n0\r\n\r\nEXTRA"), BAD_FRAMING);
    assert_eq!(run("\r\nzz\r\n"), BAD_FRAMING);
    assert_eq!(run("XX0\r\n\r\n"), BAD_FRAMING);
    assert_eq!(run("\r\n1\r\nc\r\n0\r\n\r\n"), Err(Error::BodyTooLarge));
}

#[test]
fn responses_the_client_cannot_frame_or_hold_are_refused() {
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\n\r\nbody"),
        Err(Error::UnsupportedFraming),
        "a body that ends only when the connection closes"
    );
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"),
        Err(Error::UnsupportedFraming)
    );
    // One byte more than the buffer, and the largest length there is.
    let over = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BUF + 1);
    assert_eq!(get(over.as_bytes()), Err(Error::BodyTooLarge));
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n"),
        Err(Error::BodyTooLarge),
        "not truncated to a small number"
    );
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nContent-Length: 4294967298\r\n\r\nok"),
        Err(Error::BodyTooLarge),
        "2^32 + 2 is not 2"
    );
    // With no buffer the body is discarded, up to 256 KiB.
    assert_eq!(
        get_discarding(b"HTTP/1.1 500 X\r\nContent-Length: 262145\r\n\r\n"),
        Err(Error::BodyTooLarge)
    );
    assert_eq!(
        get_discarding(b"HTTP/1.1 500 X\r\nContent-Length: 262144\r\n\r\n"),
        Err(Error::Timeout),
        "within the limit it is read, and this peer never sends it"
    );
    assert_eq!(
        get_discarding(b"HTTP/1.1 500 X\r\nContent-Length: 18446744073709551615\r\n\r\n"),
        Err(Error::BodyTooLarge)
    );
}

/// A peer that stops sending is timed out in every state that waits for it,
/// not only while the head is awaited.
#[test]
fn a_peer_that_stops_mid_response_is_timed_out_in_every_state() {
    const TIMEOUT: Result<Response, Error> = Err(Error::Timeout);
    assert_eq!(get(b"HTTP/1.1 200 OK\r\nContent-Le"), TIMEOUT, "mid head");
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhalf"),
        TIMEOUT,
        "mid body"
    );
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel"),
        TIMEOUT,
        "mid chunk"
    );
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n"),
        TIMEOUT,
        "between chunks"
    );
    assert_eq!(
        get(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nTrailer: x\r\n"),
        TIMEOUT,
        "mid trailers"
    );
    assert_eq!(
        get_discarding(b"HTTP/1.1 500 X\r\nContent-Length: 100\r\n\r\nhalf"),
        TIMEOUT,
        "mid discarded body"
    );
    assert_eq!(
        get_discarding(b"HTTP/1.1 500 X\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel"),
        TIMEOUT,
        "mid discarded chunk"
    );
    // Interim responses do not reset the clock.
    let flood = "HTTP/1.1 100 Continue\r\n\r\n".repeat(40);
    assert_eq!(get(flood.as_bytes()), TIMEOUT, "interim responses only");
    // A chunked body that fills the buffer, its end never sent.
    const CAP: usize = 8192;
    let cfg = client::Config {
        buffer_size: CAP as u32,
        ..client_cfg()
    };
    let mut reply =
        format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{CAP:x}\r\n").into_bytes();
    reply.extend(std::iter::repeat_n(b'c', CAP));
    assert_eq!(
        raw_exchange(cfg, "GET", &[], Some(RX), &reply).result,
        TIMEOUT,
        "after a full buffer"
    );
}

/// What the client sends, byte for byte, as a peer that shares no code with
/// it received it. The in-tree server parses with this crate's own parser,
/// so agreement with it proves less.
#[test]
fn the_request_on_the_wire_is_exactly_this() {
    const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    let sent = |method: &str, headers: &[(&str, &[u8])]| {
        let x = raw_exchange(client_cfg(), method, headers, Some(RX), REPLY);
        ok(200, 0)(x.result);
        String::from_utf8(x.request).unwrap()
    };
    assert_eq!(
        sent("GET", &[]),
        "GET /raw HTTP/1.1\r\nHost: raw.test\r\n\r\n"
    );
    assert_eq!(
        sent("DELETE", &[]),
        "DELETE /raw HTTP/1.1\r\nHost: raw.test\r\n\r\n"
    );
    assert_eq!(
        sent("HEAD", &[]),
        "HEAD /raw HTTP/1.1\r\nHost: raw.test\r\n\r\n"
    );
    // A method that carries a body says how long it is, even when it is empty.
    for method in ["PUT", "POST"] {
        assert_eq!(
            sent(method, &[]),
            format!("{method} /raw HTTP/1.1\r\nHost: raw.test\r\nContent-Length: 0\r\n\r\n")
        );
    }
    assert_eq!(
        sent(
            "PUT",
            &[("x-a", b"1"), ("x-b", b"two words"), ("x-a", b"3")]
        ),
        "PUT /raw HTTP/1.1\r\nHost: raw.test\r\nContent-Length: 0\r\n\
         x-a: 1\r\nx-b: two words\r\nx-a: 3\r\n\r\n",
        "the caller's headers, in the caller's order, after the client's own"
    );
}

/// A body the caller gave no buffer for is kept beside the head when it
/// fits. Its bytes, not only its length.
#[test]
fn a_discarded_body_is_kept_intact_when_it_fits_beside_the_head() {
    let mut w = world(
        11,
        Faults {
            short_send: 300_000,
            partial_recv: 300_000,
            ..Faults::default()
        },
    );
    for (i, body) in [1u32, 2, 300, 1500, 5000, 20_000].into_iter().enumerate() {
        w.server.script(Behavior::StatusWithBody(412, body));
        let req = Request {
            method: "GET",
            target: "/k",
            host: "objects.test",
            host_id: 0,
            addr: ADDR,
            headers: &[],
            body: None,
            response_buf: None,
        };
        w.client.start(&mut w.io, 500, &req).unwrap();
        w.run_until_done(i + 1);
        let r = w.done[i].result.expect("an error status is a response");
        assert_eq!((r.status, r.body_len), (412, body));
        let head_len = w.client.copy_head(&mut w.io, r.conn, RX);
        assert!(w.io.buf(RX)[..head_len as usize].ends_with(b"\r\n\r\n"));
        assert_eq!(w.client.content_length(r.conn), Some(body as u64));
        let kept = w.client.discarded_body(&mut w.io, r.conn).to_vec();
        if body <= HEAD_MAX - head_len {
            assert_eq!(kept.len(), body as usize, "body of {body}");
            assert!(
                kept.iter().all(|b| *b == b'e'),
                "body of {body}: not the bytes the server sent"
            );
        } else {
            assert!(
                kept.is_empty(),
                "body of {body} does not fit and is not kept"
            );
        }
    }
    assert_eq!(w.client.connects, 1, "discarding costs no connection");
}

/// Chunks of one and of seven bytes put a chunk header in nearly every
/// read. The larger test skips sizes this small because the whole object
/// would not fit the server's buffer.
#[test]
fn tiny_chunks_decode_to_the_stored_bytes() {
    let faults = Faults {
        short_send: 300_000,
        partial_recv: 400_000,
        delay: 100_000,
        ..Faults::default()
    };
    for seed in 0..8u64 {
        let mut w = world(seed, faults);
        let len = 2_000u32;
        w.fill_tx(len, seed as u8);
        ok(200, 0)(w.request("PUT", "/k", Some(len)));
        let sent = w.io.buf(TX)[..len as usize].to_vec();
        for chunk in [1u32, 7, 13] {
            w.io.buf(RX)[..len as usize].fill(0);
            w.server.script(Behavior::ChunkedBody(chunk));
            ok(200, len)(w.request("GET", "/k", None));
            assert!(
                w.io.buf(RX)[..len as usize] == sent[..],
                "seed {seed}, chunk size {chunk}: decoded bytes differ"
            );
        }
        assert_eq!(w.client.connects, 1);
    }
}

#[test]
fn interim_responses_101_and_malformed_1xx_are_refused_while_100_continues() {
    assert_eq!(
        get(b"HTTP/1.1 101 Switching Protocols\r\n\r\n"),
        BAD_FRAMING
    );
    for bad_1xx in [
        b"HTTP/1.1 100 Continue\r\nContent-Length: 5\r\n\r\n".as_slice(),
        b"HTTP/1.1 100 Continue\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(),
        b"HTTP/1.1 100 Continue\r\nConnection: close\r\n\r\n".as_slice(),
    ] {
        assert_eq!(get(bad_1xx), BAD_FRAMING);
    }
    ok(200, 2)(get(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
    ));
}
