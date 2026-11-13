// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP client against the in-tree server on one simulated `Io`, with heap
//! allocation forbidden while requests run.

mod common;
use common::*;

#[test]
fn put_then_get_round_trips_the_bytes() {
    let mut w = world(1, Faults::default());
    w.fill_tx(BUF, 7);
    ok(200, 0)(w.request("PUT", "/bucket/a", Some(BUF)));
    assert_eq!(w.server.object_len("/bucket/a"), Some(BUF));
    ok(200, BUF)(w.request("GET", "/bucket/a", None));
    let sent = w.io.buf(TX).to_vec();
    assert_eq!(
        &w.io.buf(RX)[..],
        &sent[..],
        "fetched bytes differ from stored bytes"
    );
    ok(200, 0)(w.request("HEAD", "/bucket/a", None));
    assert_eq!(
        w.client.header(&mut w.io, 0, "content-length"),
        Some(&b"32768"[..]),
        "HEAD reports the size without a body"
    );
    ok(204, 0)(w.request("DELETE", "/bucket/a", None));
    ok(404, 0)(w.request("GET", "/bucket/a", None));
    ok(200, 0)(w.request("PUT", "/bucket/empty", Some(0)));
    ok(200, 0)(w.request("GET", "/bucket/empty", None));
}

#[test]
fn scripted_statuses_pass_through() {
    let mut w = world(4, Faults::default());
    w.server.script(Behavior::Status(503));
    w.server.script(Behavior::Status(403));
    ok(503, 0)(w.request("GET", "/k", None));
    ok(403, 0)(w.request("GET", "/k", None));
    assert_eq!(
        w.client.connects, 1,
        "an error status does not cost the connection"
    );
}

#[test]
fn invalid_requests_are_refused_before_any_io() {
    let mut w = world(9, Faults::default());
    assert_eq!(
        w.start(1, "GET", "/has space", None),
        Err(Error::BadRequest)
    );
    assert_eq!(
        w.start(1, "GET", "/a\r\nX-Injected: 1", None),
        Err(Error::BadRequest)
    );
    let long = format!("/{}", "a".repeat(HEAD_MAX as usize));
    assert_eq!(w.start(1, "GET", &long, None), Err(Error::BadRequest));
    assert_eq!((w.client.in_flight(), w.client.connects), (0, 0));
}

#[test]
fn a_rejected_request_does_not_serve_the_previous_headers() {
    let mut w = world(10, Faults::default());
    w.fill_tx(100, 0);
    ok(200, 0)(w.request("PUT", "/k", Some(100)));
    ok(200, 0)(w.request("HEAD", "/k", None));
    assert_eq!(
        w.client.header(&mut w.io, 0, "content-length"),
        Some(&b"100"[..])
    );
    // The response head was "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n...". This
    // rejected target lays "Content-Length: 999" over the same offsets.
    let target = "/aaaaaaaaaaaaContent-Length: 999";
    assert_eq!(&format!("GET {target}")[17..36], "Content-Length: 999");
    assert_eq!(w.start(1, "GET", target, None), Err(Error::BadRequest));
    assert_eq!(
        w.client.header(&mut w.io, 0, "content-length"),
        None,
        "the head buffer now holds the rejected request"
    );
}
