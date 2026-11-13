// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP client against the in-tree server on one simulated `Io`, with heap
//! allocation forbidden while requests run.

mod common;
use common::*;

#[test]
fn one_connection_is_reused_across_requests() {
    let mut w = world(2, Faults::default());
    w.fill_tx(100, 0);
    for i in 0..5 {
        ok(200, 0)(w.request("PUT", "/k", Some(100)));
        ok(200, 100)(w.request("GET", "/k", None));
        assert_eq!(
            (w.client.connects, w.server.accepted),
            (1, 1),
            "round {i} opened another connection"
        );
    }
}

#[test]
fn connections_are_replaced_after_max_requests_even_with_free_slots() {
    // Three slots, so a spare is always available: the used-up connection
    // must still be the one that is closed and replaced.
    let mut w = world_with(
        3,
        Faults::default(),
        client::Config {
            max_requests: 2,
            ..client_cfg()
        },
    );
    w.fill_tx(10, 0);
    for round in 1..=3u64 {
        ok(200, 0)(w.request("PUT", "/k", Some(10)));
        ok(200, 0)(w.request("PUT", "/k", Some(10)));
        w.settle(20);
        assert_eq!(w.client.connects, round, "two requests per connection");
        assert_eq!((w.client.open_connections(), w.server.open_connections()), (1, 1), "round {round}: the used-up connection must have been closed, not left beside a new one");
    }
}

#[test]
fn connections_are_replaced_after_max_requests_or_max_age() {
    let mut w = world_with(
        3,
        Faults::default(),
        client::Config {
            max_requests: 2,
            max_age_ns: 500 * MS,
            ..client_cfg()
        },
    );
    w.fill_tx(10, 0);
    for _ in 0..4 {
        ok(200, 0)(w.request("PUT", "/k", Some(10)));
    }
    w.settle(50);
    assert_eq!(w.client.connects, 2, "two requests per connection");
    assert_eq!(
        (w.server.accepted, w.server.closed),
        (2, 1),
        "the used-up connection was closed, not left beside a new one"
    );
    ok(200, 10)(w.request("GET", "/k", None));
    w.settle(50);
    assert_eq!(
        (w.server.accepted, w.server.closed),
        (3, 2),
        "a used-up connection is replaced before a free slot is opened"
    );
}

#[test]
fn an_aged_connection_is_replaced_before_a_free_slot_is_used() {
    // The periodic sweep is effectively off (age far above the settle time
    // between ticks would still catch it), so shorten the path: the request
    // arrives in the same tick interval in which the connection expires.
    let mut w = world_with(
        3,
        Faults::default(),
        client::Config {
            max_age_ns: 500 * MS,
            ..client_cfg()
        },
    );
    w.fill_tx(10, 0);
    ok(200, 0)(w.request("PUT", "/k", Some(10)));
    // Advance virtual time past the age limit without running a client tick.
    let until = w.io.now_ns() + 600 * MS;
    while w.io.now_ns() < until {
        w.io.tick(true, &mut w.out).unwrap();
        while let Some(c) = w.out.pop() {
            if c.token.0 == HEARTBEAT {
                w.io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                    .unwrap();
            } else if !matches!(
                w.client.on_completion(&mut w.io, c),
                Handled::Pending | Handled::Done(_)
            ) {
                w.server.on_completion(&mut w.io, c);
            }
        }
    }
    assert_eq!(
        w.client.open_connections(),
        1,
        "still open: nothing has swept it"
    );
    ok(200, 10)(w.request("GET", "/k", None));
    w.settle(20);
    assert_eq!(w.client.connects, 2);
    assert_eq!(
        (w.client.open_connections(), w.server.open_connections()),
        (1, 1),
        "the aged connection was replaced, not kept beside a new one"
    );
}

#[test]
fn idle_connections_are_closed_once_they_pass_their_maximum_age() {
    let mut w = world_with(
        3,
        Faults::default(),
        client::Config {
            max_age_ns: 500 * MS,
            ..client_cfg()
        },
    );
    w.fill_tx(10, 0);
    ok(200, 0)(w.request("PUT", "/k", Some(10)));
    w.settle(300);
    assert_eq!(
        (w.client.open_connections(), w.server.open_connections()),
        (1, 1),
        "younger than the limit: kept"
    );
    w.settle(400);
    assert_eq!(
        (w.client.open_connections(), w.server.open_connections()),
        (0, 0),
        "older than the limit: closed with no request pending"
    );
    ok(200, 10)(w.request("GET", "/k", None));
    assert_eq!(
        w.client.connects, 2,
        "the next request opens a new connection"
    );
}

#[test]
fn idle_connections_past_max_age_are_closed_without_a_request() {
    let mut w = world_with(
        11,
        Faults::default(),
        client::Config {
            max_age_ns: 500 * MS,
            ..client_cfg()
        },
    );
    ok(404, 0)(w.request("GET", "/k", None));
    w.settle(100);
    assert_eq!(
        (w.server.accepted, w.server.closed),
        (1, 0),
        "a young idle connection stays open"
    );
    w.settle(500);
    assert_eq!(
        (w.server.accepted, w.server.closed),
        (1, 1),
        "the aged idle connection was closed by tick"
    );
    ok(404, 0)(w.request("GET", "/k", None));
    assert_eq!(
        w.client.connects, 2,
        "the slot was freed and a new connection opened"
    );
}

#[test]
fn busy_when_every_connection_is_in_use() {
    let mut w = world(8, Faults::default());
    for _ in 0..CONNS {
        w.server.script(Behavior::Stall);
    }
    for t in 0..CONNS as u64 {
        w.start(t, "GET", "/k", None).unwrap();
    }
    assert_eq!(w.start(99, "GET", "/k", None), Err(Error::Busy));
    w.run_until_done(CONNS as usize);
    assert!(w.done.iter().all(|d| d.result == Err(Error::Timeout)));
    w.settle(50);
    assert_eq!(
        w.start(100, "GET", "/k", None),
        Ok(()),
        "slots are usable again after the timeouts"
    );
}

#[test]
fn concurrent_requests_use_separate_connections() {
    let mut w = world(10, Faults::default());
    w.fill_tx(5000, 3);
    ok(200, 0)(w.request("PUT", "/k", Some(5000)));
    // HEAD has no response body, so three can run at once without sharing a buffer.
    for t in 0..CONNS as u64 {
        w.start(200 + t, "HEAD", "/k", None).unwrap();
    }
    let before = w.done.len();
    w.run_until_done(before + CONNS as usize);
    assert!(w.done[before..].iter().all(|d| d.result
        == Ok(Response {
            status: 200,
            body_len: 0,
            conn: d.result.unwrap().conn
        })));
    assert_eq!(w.client.connects, CONNS as u64);
}

/// A server may close a connection while it sits idle. With the idle check
/// the client notices before sending and uses a new connection, so the
/// request simply succeeds. Without it the request is sent into a dead
/// connection and fails.
#[test]
fn a_connection_closed_while_idle_is_replaced_before_it_is_used() {
    // A scripted reply is sent as it is and the server then closes, without
    // saying so in the response.
    const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    let mut w = world(31, Faults::default());
    w.server.script(Behavior::Raw(REPLY));
    ok(200, 2)(w.request("GET", "/k", None));
    w.settle(20);
    ok(404, 0)(w.request("GET", "/missing", None));
    assert_eq!(
        (w.client.idle_closed, w.client.idle_dirty, w.client.connects),
        (1, 0, 2)
    );

    let mut w = world_with(
        31,
        Faults::default(),
        client::Config {
            check_idle: false,
            ..client_cfg()
        },
    );
    w.server.script(Behavior::Raw(REPLY));
    ok(200, 2)(w.request("GET", "/k", None));
    w.settle(20);
    assert!(
        w.request("GET", "/missing", None).is_err(),
        "without the check the request goes into the closed connection"
    );
}
