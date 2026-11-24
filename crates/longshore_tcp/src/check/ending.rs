// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How connections and listeners end, on every backend: a peer that
//! sends, shuts down and waits is answered, and a listener's slot comes
//! back after its close.

use super::{fail, must, prefix, prefixed, World};
use crate::{Addr, Conn, Event, ListenConfig, Why};
use longshore_common::io::Io;

/// What one side has been told so far.
#[derive(Default)]
struct Seen {
    finished: bool,
    ended: Option<Why>,
    frames: Vec<Vec<u8>>,
}

/// Turn until `done`; events of the server go to the first `Seen`, of
/// the client to the second, and frames are pulled at each `Readable`.
fn watch<IO: Io>(
    w: &mut World<IO>,
    (c, s): (Conn, Conn),
    seen: &mut (Seen, Seen),
    done: impl Fn(&(Seen, Seen)) -> bool,
) {
    if done(seen) {
        return;
    }
    w.until(|w, at_server, e| {
        let (net, io) = if at_server { w.server() } else { w.client() };
        let (side, conn) = if at_server {
            (&mut seen.0, s)
        } else {
            (&mut seen.1, c)
        };
        match e {
            Event::Readable(got) if got == conn => {
                while let Some(f) = must(net.next_frame(io, conn), "next_frame") {
                    match net.bytes(io, &f) {
                        Some(bytes) => side.frames.push(bytes.to_vec()),
                        None => fail(format_args!("a frame just pulled has no bytes")),
                    }
                    net.release(f);
                }
            }
            Event::Finished(got) if got == conn => side.finished = true,
            Event::Closed { conn: got, why } if got == conn => side.ended = Some(why),
            Event::Writable(_) => {}
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Finished(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => fail(format_args!("unexpected {e:?}")),
        }
        done(seen)
    });
}

/// The peer sends a request and shuts its write side down; the server,
/// whose connections are half-close, is told so after the request,
/// answers, and shuts down in turn. Both ends then end in order.
pub(super) fn answered_after_shutdown<IO: Io>(
    world: &mut impl FnMut(&Addr) -> World<IO>,
    addr: Addr,
) -> Vec<Vec<u8>> {
    let mut w = world(&addr);
    let l = w.listen(addr, ListenConfig::new(prefix().with_half_close(true)));
    let to = w.dial_addr(l, addr);
    let (c, s) = w.connect(to, prefix());
    let (request, answer) = (prefixed(900, 21), prefixed(2100, 22));
    if !w.send(c, &request) {
        fail(format_args!("queue full"));
    }
    let (client, io) = w.client();
    must(client.shutdown_write(io, c), "client shutdown");
    let mut seen = (Seen::default(), Seen::default());
    watch(&mut w, (c, s), &mut seen, |seen| seen.0.finished);
    if seen.0.frames != [request.clone()] || seen.0.ended.is_some() {
        fail(format_args!(
            "the peer finished before its request was pulled"
        ));
    }
    let (server, server_io) = w.server();
    must(server.send_bytes(server_io, s, &answer), "answer");
    must(server.shutdown_write(server_io, s), "server shutdown");
    watch(&mut w, (c, s), &mut seen, |seen| {
        seen.0.ended.is_some() && seen.1.ended.is_some()
    });
    let ends = (seen.0.ended, seen.1.ended);
    if ends != (Some(Why::Orderly), Some(Why::Orderly)) || seen.1.frames != [answer.clone()] {
        fail(format_args!(
            "ends {ends:?}, {} answers",
            seen.1.frames.len()
        ));
    }
    vec![request, answer]
}

/// A listener is closed and another opened, over and over, in a set with
/// two listener slots: every slot comes back, which needs each backend to
/// report the end of the closed listener's accept.
pub(super) fn listener_slots_come_back<IO: Io>(
    world: &mut impl FnMut(&Addr) -> World<IO>,
    addr: Addr,
) {
    let mut w = world(&addr);
    for round in 0..6 {
        if let Addr::Unix(path) = &addr {
            // The path of the listener before; usually there is none.
            drop(std::fs::remove_file(path.as_str()));
        }
        let mut listener = None;
        for _ in 0..4000 {
            match w.try_listen(addr, ListenConfig::new(prefix())) {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                // Its predecessors are still closing.
                Err(_) => w.turn(),
            }
        }
        let Some(l) = listener else {
            fail(format_args!("round {round}: no listener slot came back"));
        };
        let to = w.dial_addr(l, addr);
        let (c, s) = w.connect(to, prefix());
        let got = w.exchange(c, s, &[prefixed(64, round)]);
        if got != [prefixed(64, round)] {
            fail(format_args!("round {round}: the frame did not arrive"));
        }
        let (server, io) = w.server();
        must(server.close_listener(io, l), "close_listener");
        must(server.close(io, s), "close");
        let (client, client_io) = w.client();
        must(client.close(client_io, c), "close");
    }
}
