// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A pool that is empty refuses or pauses, loses nothing, and whoever
//! waited goes on when there is room, with no further call of its own.
//!
//! One scenario for each pool of the exhaustion table; those already
//! tested elsewhere are named here:
//!
//! | Pool | Scenario |
//! |---|---|
//! | Connection slots | `a_connection_slot_comes_back_when_its_connection_has_ended`, here |
//! | Accept range | `accepts_wait_after_a_failure_and_go_on_when_a_slot_is_free`, `the_wait_after_a_failed_accept_doubles_while_accepts_keep_failing` |
//! | Frame memory | `reading_stopped_for_want_of_frame_memory_goes_on_once_the_input_is_pulled`, `an_end_whose_last_frame_finds_no_frame_memory_waits_for_it_and_loses_nothing` |
//! | Frame memory, among connections | `as_many_connections_as_frame_buffers_all_deliver`, here |
//! | Hold count of one buffer | `a_frame_the_backlog_cannot_count_another_hold_for_is_pulled_later` |
//! | Send segments | `a_connection_refused_for_want_of_a_segment_is_told_when_one_returns`, `a_segment_its_waiter_never_took_goes_to_the_next_one_waiting` |
//! | Send queue of a connection | `a_full_queue_refuses_without_losing_anything_and_says_when_there_is_room` |
//! | Buffer ring | `one_connections_unread_input_does_not_keep_the_ring_from_others` |
//! | The driver's operation slots | `a_driver_with_too_few_operation_slots_slows_the_set_down_and_loses_nothing`, here |

use super::{prefix, prefixed, send, sizes, SimNet, World, ADDR};
use longshore_common::io::{Completions, Io, Limits};
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::{Config, Conn, Event, KernelTransport, ListenConfig, Listener, Net, NetError};

#[test]
fn a_connection_slot_comes_back_when_its_connection_has_ended() {
    let mut w = World::new(51, Faults::NONE);
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let mut open = Vec::new();
    for _ in 0..4 {
        open.push(w.connect(prefix()).0);
    }
    assert_eq!(
        w.client.connect(&mut w.io, ADDR, prefix()),
        Err(NetError::NoSlots)
    );
    // One ends. Its slot is free once its close and whatever was in
    // flight have ended; nothing else is asked of the caller.
    w.client.close(&mut w.io, open[2]).unwrap();
    let mut again = None;
    for _ in 0..200 {
        w.turn();
        while w.server.next_event().is_some() {}
        if let Ok(conn) = w.client.connect(&mut w.io, ADDR, prefix()) {
            again = Some(conn);
            break;
        }
    }
    let conn = again.expect("the slot never came back");
    assert!(!open.contains(&conn), "a handle was given out twice");
}

#[test]
fn as_many_connections_as_frame_buffers_all_deliver() {
    // Three frame buffers a side, and three connections.
    let few =
        |connects, accepts, listeners| sizes(connects, accepts, listeners).with_frames(3, 8192);
    let mut w = World::with(52, Faults::NONE, few(3, 0, 0), few(0, 3, 1));
    let _: Listener = w.listen(ListenConfig::new(prefix()));
    let pairs: Vec<(Conn, Conn)> = (0..3).map(|_| w.connect(prefix())).collect();
    // Each sends frames longer than a ring buffer, and nobody pulls: the
    // input of each is moved out of the ring, into a frame buffer.
    let mut queued = [0u8; 3];
    for _ in 0..400 {
        for (i, (c, _)) in pairs.iter().enumerate() {
            if queued[i] < 3 && send(&mut w, *c, &prefixed(3000, queued[i])) {
                queued[i] += 1;
            }
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while w.server.next_event().is_some() {}
    }
    assert_eq!(queued, [3, 3, 3]);
    // Now the caller pulls. Each connection holds a buffer and has a
    // frame that continues past it: all of them must still get through.
    let mut got = [0usize; 3];
    for _ in 0..3000 {
        for (i, (_, s)) in pairs.iter().enumerate() {
            for (frame, bytes) in w.pull_all(*s) {
                assert_eq!(bytes, prefixed(3000, got[i] as u8), "connection {i}");
                got[i] += 1;
                w.server.release(frame);
            }
        }
        if got == [3, 3, 3] {
            break;
        }
        w.turn();
        while w.client.next_event().is_some() {}
        while w.server.next_event().is_some() {}
    }
    assert_eq!(
        got,
        [3, 3, 3],
        "connections waited on each other for frame memory"
    );
}

/// One turn of two sets on `io`, without the harness's own timer, which
/// would want an operation slot too.
#[cfg(test)]
fn pump(io: &mut SimIo, client: &mut SimNet, server: &mut SimNet, out: &mut Completions) {
    let now = io.now_ns();
    client.poll(io, now);
    server.poll(io, now);
    io.tick(false, out).unwrap();
    while let Some(c) = out.pop() {
        let taken = client.on_completion(io, c) || server.on_completion(io, c);
        assert!(taken, "a completion nobody claimed: {c:?}");
    }
}

#[test]
fn a_driver_with_too_few_operation_slots_slows_the_set_down_and_loses_nothing() {
    // Three connections each way are planned some twenty operation slots.
    // Seven are taken for good once everything is up: the listener's
    // accept and a receive for each of six connection ends. The driver
    // is built with eight: one slot for every connect, send and close.
    let base = Limits::BASE.with_sockets_max(1);
    let (l1, ccfg) = Config::plan(base, sizes(3, 0, 0)).unwrap();
    let after = sizes(0, 3, 1).with_token_base(ccfg.token_base() + ccfg.tokens());
    let (limits, scfg) = Config::plan(l1, after).unwrap();
    let mut io = SimIo::new(limits.with_ops_max(8), 53, Faults::NONE);
    let mut client = Net::new(KernelTransport::new(&ccfg), ccfg, &limits);
    let mut server = Net::new(KernelTransport::new(&scfg), scfg, &limits);
    let listener = server
        .listen(&mut io, ADDR, ListenConfig::new(prefix()))
        .unwrap();
    let conns: Vec<Conn> = (0..3)
        .map(|_| client.connect(&mut io, ADDR, prefix()).unwrap())
        .collect();
    let frame = prefixed(1500, 9);
    let (mut sent, mut got, mut ended) = ([0u8; 3], 0, 0);
    let mut out = Completions::new(16);
    for _ in 0..20_000 {
        pump(&mut io, &mut client, &mut server, &mut out);
        for (i, conn) in conns.iter().enumerate() {
            if sent[i] < 4 && client.send_bytes(&mut io, *conn, &frame).is_ok() {
                sent[i] += 1;
            } else if sent[i] == 4 && client.send_queue(*conn) == 0 && client.is_up(*conn) {
                // Everything went: the connection is closed, and counted once.
                client.close(&mut io, *conn).unwrap();
            }
        }
        while client.next_event().is_some() {}
        while let Some(e) = server.next_event() {
            match e {
                Event::Readable(conn) => {
                    while let Some(f) = server.next_frame(&mut io, conn).unwrap() {
                        assert_eq!(server.bytes(&mut io, &f), Some(&frame[..]));
                        server.release(f);
                        got += 1;
                    }
                }
                Event::Closed { .. } => ended += 1,
                Event::Accepted { .. } | Event::Writable(_) => {}
                Event::Up(_)
                | Event::Refused { .. }
                | Event::Finished(_)
                | Event::Down { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => panic!("unexpected {e:?}"),
            }
        }
        if ended == 3 {
            break;
        }
    }
    assert_eq!((got, ended), (12, 3));
    // The listener goes too, and then both sets hold nothing.
    server.close_listener(&mut io, listener).unwrap();
    for _ in 0..200 {
        pump(&mut io, &mut client, &mut server, &mut out);
    }
    assert_eq!((client.next_event(), server.next_event()), (None, None));
    assert!(client.usage().is_idle(), "{:?}", client.usage());
    assert!(server.usage().is_idle(), "{:?}", server.usage());
}
