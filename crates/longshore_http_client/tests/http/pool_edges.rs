// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The connection pool where its other tests do not go: a server that says
//! it will close, more than one host, retiring connections, and what a
//! failure on a first use reports.

#[cfg(test)]
mod common;
use common::*;
use longshore_http_client::head::HttpError;
use longshore_http_client::server::Behavior;

#[cfg(test)]
fn request_to(w: &mut World, host_id: u16, host: &str, method: &str, target: &str) -> Response {
    let req = Request {
        method,
        target,
        host,
        host_id,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    let n = w.done.len();
    w.client.start(&mut w.io, 900 + n as u64, &req).unwrap();
    w.run_until_done(n + 1);
    w.done[n].result.expect("a response")
}

/// A response that announces the close is not followed by a reuse. The idle
/// check would hide a client that forgot: it would find the connection dead
/// and replace it. So the counters, and a run with the check off, decide.
#[test]
fn a_connection_the_server_said_it_will_close_is_not_reused() {
    const HTTP10: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok";
    for (behavior, body) in [(Behavior::Chunked, 0), (Behavior::Raw(HTTP10), 2)] {
        for check_idle in [true, false] {
            let mut w = world_with(
                40,
                Faults::default(),
                client::Config {
                    check_idle,
                    ..client_cfg()
                },
            );
            w.server.script(behavior);
            ok(200, body)(w.request("GET", "/k", None));
            w.settle(20);
            assert_eq!(
                w.client.open_connections(),
                0,
                "the client closed its end ({behavior:?})"
            );
            ok(404, 0)(w.request("GET", "/missing", None));
            assert_eq!(
                (w.client.connects, w.client.idle_closed, w.client.idle_dirty),
                (2, 0, 0),
                "{behavior:?}, idle check {check_idle}: nothing was left for the check to find"
            );
        }
    }
}

/// A connection to one host is never used for a request to another.
#[test]
fn hosts_do_not_share_connections() {
    let mut w = world(41, Faults::default());
    let first = request_to(&mut w, 0, "objects.test", "GET", "/missing");
    let second = request_to(&mut w, 1, "other.test", "GET", "/missing");
    assert_eq!((first.status, second.status), (404, 404));
    assert_ne!(first.conn, second.conn);
    assert_eq!((w.client.connects, w.client.open_connections()), (2, 2));
    // Each host goes back to its own connection.
    assert_eq!(
        request_to(&mut w, 1, "other.test", "GET", "/missing").conn,
        second.conn
    );
    assert_eq!(
        request_to(&mut w, 0, "objects.test", "GET", "/missing").conn,
        first.conn
    );
    assert_eq!(w.client.connects, 2);
}

/// With every slot idle on one host, a request for another host takes one
/// of them: the old connection is closed and a new one opened.
#[test]
fn a_full_pool_gives_up_an_idle_connection_to_another_host() {
    let mut w = world(42, Faults::default());
    for t in 0..u64::from(CONNS) {
        w.start(200 + t, "HEAD", "/missing", None).unwrap();
    }
    w.run_until_done(CONNS as usize);
    let conns: std::collections::BTreeSet<u32> =
        w.done.iter().map(|d| d.result.unwrap().conn).collect();
    let tokens: std::collections::BTreeSet<u64> = w.done.iter().map(|d| d.token).collect();
    assert_eq!(conns.into_iter().collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(tokens.into_iter().collect::<Vec<_>>(), [200, 201, 202]);
    assert!(w.done.iter().all(|d| d.result.unwrap().status == 404));
    assert_eq!(w.client.connects, u64::from(CONNS));

    let r = request_to(&mut w, 1, "other.test", "GET", "/missing");
    assert_eq!(r.status, 404);
    w.settle(20);
    assert_eq!(w.client.connects, u64::from(CONNS) + 1);
    assert_eq!(w.client.open_connections(), CONNS as usize);
    assert_eq!(
        w.server.closed(),
        1,
        "the server saw the old connection close"
    );
}

#[test]
fn retired_connections_finish_their_request_and_are_not_reused() {
    let mut w = world(43, Faults::default());
    for t in 0..u64::from(CONNS) {
        w.start(200 + t, "HEAD", "/missing", None).unwrap();
    }
    w.run_until_done(CONNS as usize);
    assert_eq!(
        w.client.retire_some(1, 5),
        0,
        "another host's are untouched"
    );
    assert_eq!(w.client.retire_some(0, 0), 0);
    assert_eq!(w.client.retire_some(0, 2), 2);
    assert_eq!(
        w.client.retire_some(0, 5),
        1,
        "only the one not yet retired"
    );
    assert_eq!(w.client.retire_some(0, 5), 0);
    ok(404, 0)(w.request("GET", "/missing", None));
    assert_eq!(
        w.client.connects,
        u64::from(CONNS) + 1,
        "a retired connection is replaced"
    );

    // Retiring while a request is in flight does not disturb that request.
    let mut in_flight = world(43, Faults::default());
    ok(404, 0)(in_flight.request("GET", "/missing", None));
    in_flight.start(300, "GET", "/missing", None).unwrap();
    assert_eq!(in_flight.client.retire_some(0, 1), 1);
    in_flight.run_until_done(2);
    ok(404, 0)(in_flight.done[1].result);
    assert_eq!(in_flight.client.connects, 1);
    ok(404, 0)(in_flight.request("GET", "/missing", None));
    assert_eq!(in_flight.client.connects, 2);

    // Unmeasured connections: this one was opened to an address that
    // reports no segment size.
    let mut unmeasured = world(43, Faults::default());
    ok(404, 0)(unmeasured.request("GET", "/missing", None));
    assert_eq!(unmeasured.client.retire_unmeasured(1), 0);
    assert_eq!(unmeasured.client.retire_unmeasured(0), 1);
    assert_eq!(unmeasured.client.retire_unmeasured(0), 0, "already retired");
}

/// `reused` tells a retry layer whether a close is the usual stale
/// keep-alive or a failure on a new connection. Both values, exactly.
#[test]
fn a_close_reports_whether_the_connection_had_been_used() {
    for behavior in [Behavior::CloseBeforeResponse, Behavior::TruncateBody] {
        let mut w = world(5, Faults::default());
        w.fill_tx(1000, 0);
        if matches!(behavior, Behavior::TruncateBody) {
            // Needs an object to truncate; store it on a connection that is
            // then retired so the failing request opens its own.
            ok(200, 0)(w.request("PUT", "/k", Some(1000)));
            assert_eq!(w.client.retire_some(0, 1), 1);
        }
        w.server.script(behavior);
        assert_eq!(
            w.request("GET", "/k", None),
            Err(Error::Closed { reused: false }),
            "{behavior:?} on a connection's first request"
        );
        w.settle(20);
        ok(
            if matches!(behavior, Behavior::TruncateBody) {
                200
            } else {
                404
            },
            if matches!(behavior, Behavior::TruncateBody) {
                1000
            } else {
                0
            },
        )(w.request("GET", "/k", None));
        w.server.script(behavior);
        assert_eq!(
            w.request("GET", "/k", None),
            Err(Error::Closed { reused: true }),
            "{behavior:?} on a reused connection"
        );
        assert_eq!(w.client.in_flight(), 0);
    }
}

/// The owner learns which address could not be reached, and only when the
/// connection never opened.
#[test]
fn only_a_failed_connect_names_the_address() {
    let mut w = world(
        7,
        Faults {
            connect_refused: 1_000_000,
            ..Faults::default()
        },
    );
    assert_eq!(w.client.connect_failure, None);
    assert_eq!(
        w.request("GET", "/k", None),
        Err(Error::Io(longshore_common::io::ECONNREFUSED))
    );
    assert_eq!(w.client.connect_failure.take(), Some(ADDR));

    let mut answered = world(7, Faults::default());
    answered.server.script(Behavior::GarbageHead);
    assert_eq!(
        answered.request("GET", "/k", None),
        Err(Error::Head(HttpError::BadStartLine))
    );
    assert_eq!(
        answered.client.connect_failure, None,
        "the address answered; it is the response that was bad"
    );
    answered.server.script(Behavior::Stall);
    assert_eq!(answered.request("GET", "/k", None), Err(Error::Timeout));
    assert_eq!(
        answered.client.connect_failure, None,
        "a timeout after connecting"
    );
}
