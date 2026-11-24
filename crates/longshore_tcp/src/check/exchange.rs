// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The exchanges every backend must carry alike.

use super::{fail, must, prefix, prefixed, World};
use crate::{Addr, ConnOpts, Event, Framing, HeaderFraming, ListenConfig, Rx, Why};
use longshore_common::io::{Io, Limits};
use std::net::SocketAddr;

/// Whole frames with a length prefix, in ring mode and in direct mode.
fn frames_both_ways<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: Addr) -> Vec<Vec<u8>> {
    let lens = [0usize, 1, 3, 500, 1019, 1020, 1021, 2500, 4000, 7, 0, 3000];
    let frames: Vec<Vec<u8>> = lens.iter().map(|l| prefixed(*l, 5)).collect();
    let mut trace = Vec::new();
    for rx in [Rx::Ring, Rx::Direct] {
        let mut w = World::listening_on(make, &addr);
        let (c, s) = w.pair(addr, prefix().with_rx(rx));
        let got = w.exchange(c, s, &frames);
        if got != frames {
            fail(format_args!("{rx:?}: frames differ from what was sent"));
        }
        trace.extend(got);
    }
    trace
}

/// A 256-byte header whose little-endian u32 at offset 40 is the frame's
/// whole length, received straight into frame memory.
fn header_with_size<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: Addr) -> Vec<Vec<u8>> {
    let framing = must(HeaderFraming::new(256, 40, 4, false, true, 8192), "framing");
    let opts = ConnOpts::new(Framing::Header(framing)).with_rx(Rx::Direct);
    let frame = |size: u32, seed: u8| {
        let mut f: Vec<u8> = (0..size).map(|i| (i as u8) ^ seed).collect();
        f[40..44].copy_from_slice(&size.to_le_bytes());
        f
    };
    let frames = vec![frame(256, 1), frame(257, 2), frame(4096, 3), frame(1000, 4)];
    let mut w = World::listening_on(make, &addr);
    let (c, s) = w.pair(addr, opts);
    let got = w.exchange(c, s, &frames);
    if got != frames {
        fail(format_args!("frames differ from what was sent"));
    }
    got
}

/// Raw bytes arrive complete and in order, whatever pieces they come in.
fn raw_bytes<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: Addr) -> Vec<Vec<u8>> {
    let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let pieces: Vec<Vec<u8>> = data.chunks(1000).map(<[u8]>::to_vec).collect();
    let mut trace = Vec::new();
    for rx in [Rx::Ring, Rx::Direct] {
        let mut w = World::listening_on(make, &addr);
        let (c, s) = w.pair(addr, ConnOpts::new(Framing::Bytes).with_rx(rx));
        let got: Vec<u8> = w.exchange(c, s, &pieces).concat();
        if got != data {
            fail(format_args!("{rx:?}: bytes differ from what was sent"));
        }
        trace.push(got);
    }
    trace
}

/// The peer finishes after its last frame: the frames come first, then an
/// orderly end, and a listener with peer addresses knows where it came from.
fn orderly_end_and_peer_address<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: Addr) {
    let mut w = World::listening_on(make, &addr);
    let l = w.listen(addr, ListenConfig::new(prefix()).with_peer_addr(true));
    let to = w.dial_addr(l, addr);
    let (c, s) = w.connect(to, prefix());
    if let Addr::Ip(listening) = to {
        let peer = w.server.peer(&w.io, s);
        if peer.map(|p| p.ip()) != Some(listening.ip()) {
            fail(format_args!(
                "peer address {peer:?} of a connection to {listening}"
            ));
        }
    }
    for i in 0..3 {
        if !w.send(c, &prefixed(100, i)) {
            fail(format_args!("queue full"));
        }
    }
    must(w.client.shutdown_write(&mut w.io, c), "shutdown_write");
    let (mut frames, mut ended) = (0, None);
    w.until(|w, server, e| {
        match e {
            Event::Readable(conn) if server && conn == s && ended.is_none() => {
                frames += w.pull_all(conn).len();
            }
            Event::Closed { conn, why } if server && conn == s => ended = Some(why),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => fail(format_args!("unexpected {e:?}")),
        }
        ended.is_some()
    });
    if (frames, ended) != (3, Some(Why::Orderly)) {
        fail(format_args!("{frames} frames, then {ended:?}"));
    }
}

/// Every check, on TCP at `addr` (port 0 lets the system choose) and on a
/// Unix-domain socket at `unix_path`. Returns the frames the servers
/// received over TCP, in order.
pub fn run_all<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
    unix_path: &str,
) -> Vec<Vec<u8>> {
    let tcp = Addr::Ip(addr);
    let mut trace = frames_both_ways(make, tcp);
    trace.extend(header_with_size(make, tcp));
    trace.extend(raw_bytes(make, tcp));
    orderly_end_and_peer_address(make, tcp);
    // The same over a Unix-domain socket; each world binds the path anew.
    let unix = must(Addr::unix(unix_path), "unix path");
    let mut over_unix = frames_both_ways(make, unix);
    over_unix.extend(header_with_size(make, unix));
    over_unix.extend(raw_bytes(make, unix));
    if over_unix != trace {
        fail(format_args!(
            "a Unix-domain socket carried other frames than TCP"
        ));
    }
    orderly_end_and_peer_address(make, unix);
    drop(std::fs::remove_file(unix_path));
    trace
}
