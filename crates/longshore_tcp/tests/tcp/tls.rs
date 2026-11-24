// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS through `Net`: the same frames as without it on every backend, a
//! log and its replicas without allocating, and what happens when the
//! handshake or the end of a connection goes wrong.

#![cfg(feature = "tls-core")]

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{Io, Limits};
use longshore_common::sim::{DiskConfig, Faults, SimIo};
use longshore_tcp::check::storage::Replicated;
use longshore_tcp::check::{self, prefix, secure, Certs, World};
use longshore_tcp::{
    Addr, Conn, ConnOpts, Event, Framing, HeaderFraming, ListenConfig, NetError, Rx, Why,
};
use longshore_tls::handshake::{ClientSetup, ServerSetup};
use std::net::SocketAddr;

#[cfg(test)]
#[path = "tls/ending.rs"]
mod ending;

#[global_allocator]
static ALLOC: Guard = Guard;

#[cfg(test)]
const NAME: &str = "replica.longshore.test";

/// A certificate for `NAME`, which the client expects to be `expected`'s.
#[cfg(test)]
fn certs(expected: &str) -> Certs {
    let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
    let (cert, key) = (k.cert.der().to_vec(), k.signing_key.serialize_der());
    let client = ClientSetup::with_roots(&[&cert]).unwrap();
    let server = ServerSetup::new(&[&cert], &key).unwrap();
    Certs::new(client, server, expected)
}

#[cfg(test)]
fn unix_path(tag: &str) -> String {
    let name = format!("longshore-tcp-{tag}-{}.sock", std::process::id());
    std::env::temp_dir()
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

#[cfg(test)]
fn sim_addr() -> SocketAddr {
    "192.0.2.7:7000".parse().unwrap()
}

#[cfg(test)]
fn sim(limits: Limits, seed: u64, faults: Faults) -> SimIo {
    let mut io = SimIo::new(limits, seed, faults);
    io.setup_disk(
        DiskConfig::BASE
            .with_max_files(4)
            .with_max_dirs(4)
            .with_max_bytes_per_file(1 << 20)
            .with_max_name_len(256),
    );
    io
}

/// A small world on the simulator: two connections out, `accepts` in.
#[cfg(test)]
fn world(certs: &Certs, accepts: u32) -> World<SimIo> {
    World::new_tls(
        &mut |l| sim(l, 5, Faults::NONE),
        Limits::BASE,
        secure::sizes(2, 0, 0),
        secure::sizes(0, accepts, 1),
        certs,
    )
}

/// Turn until the client's `conn` is closed; why. Nothing else may happen
/// but `conn` coming up first.
#[cfg(test)]
fn client_closed(w: &mut World<SimIo>, conn: Conn) -> Why {
    let mut ended = None;
    w.until(|_, server, e| {
        match e {
            Event::Up(c) if !server && c == conn => {}
            Event::Closed { conn: c, why } if !server && c == conn => ended = Some(why),
            Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Readable(_)
            | Event::Writable(_)
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Drained => panic!("unexpected {e:?} (server: {server})"),
        }
        ended.is_some()
    });
    ended.unwrap()
}

#[test]
fn tls_carries_the_same_frames_on_the_simulator_and_on_real_sockets() {
    let certs = certs(NAME);
    let plain = check::run_all(
        &mut |l| SimIo::new(l, 11, Faults::NONE),
        sim_addr(),
        &unix_path("plain"),
    );
    let on_sim = check::run_all_tls(
        &mut |l| SimIo::new(l, 11, Faults::NONE),
        sim_addr(),
        &unix_path("simtls"),
        &certs,
    );
    let on_sockets = check::run_all_tls(
        &mut |l| PosixIo::new(l).unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        &unix_path("posixtls"),
        &certs,
    );
    assert!(
        plain == on_sim,
        "TLS on the simulator delivered other frames"
    );
    assert!(
        plain == on_sockets,
        "TLS on real sockets delivered other frames"
    );
}

#[test]
fn tls_holds_under_partial_receives_and_short_sends() {
    let certs = certs(NAME);
    for seed in 0..10 {
        let faults = Faults::NONE
            .with_partial_recv(400_000)
            .with_short_send(300_000);
        let trace = check::run_all_tls(
            &mut |l| SimIo::new(l, seed, faults),
            sim_addr(),
            &unix_path("tlsfaults"),
            &certs,
        );
        assert!(!trace.is_empty(), "seed {seed}");
    }
}

#[test]
fn a_log_and_its_replicas_over_tls_allocate_nothing_after_the_handshakes() {
    let certs = certs(NAME);
    let w = World::new_tls(
        &mut |l| sim(l, 3, Faults::NONE),
        Limits::BASE.with_files_max(1),
        secure::sizes(4, 0, 0),
        secure::sizes(2, 4, 2),
        &certs,
    );
    let mut r = Replicated::over(w, Addr::Ip(sim_addr()), "/log.dat");
    r.round(0);
    sealed(|| (1..100).for_each(|i| r.round(i)));
}

#[test]
fn a_peer_that_never_starts_the_handshake_is_dropped_unannounced() {
    let certs = certs(NAME);
    let mut w = world(&certs, 1);
    let opts = prefix().with_handshake_timeout_ns(5_000_000);
    let addr = Addr::Ip(sim_addr());
    let l = w.listen(addr, ListenConfig::new(opts));
    let to = w.dial_addr(l, addr);
    // A plain connection: connected, and then silent.
    let (client, io) = w.client();
    let silent = client.connect(io, to, prefix()).unwrap();
    // The server closes it when the handshake's time is up, and says
    // nothing of it to its own caller.
    assert_eq!(client_closed(&mut w, silent), Why::Orderly);
    // Its slot, the only one, is free for a real peer.
    let (c, s) = w.connect(to, opts);
    let got = w.exchange(c, s, &[check::prefixed(100, 1)]);
    assert_eq!(got, [check::prefixed(100, 1)]);
}

#[test]
fn a_certificate_for_another_name_ends_the_connect_with_a_tls_failure() {
    let certs = certs("someone-else.longshore.test");
    let mut w = world(&certs, 1);
    let addr = Addr::Ip(sim_addr());
    let l = w.listen(addr, ListenConfig::new(prefix()));
    let to = w.dial_addr(l, addr);
    let (client, io) = w.client();
    let conn = client
        .connect_tls(io, to, "someone-else.longshore.test", prefix())
        .unwrap();
    assert_eq!(client_closed(&mut w, conn), Why::Tls);
}

#[test]
fn an_end_without_a_close_alert_is_a_broken_connection() {
    for rx in [Rx::Ring, Rx::Direct] {
        let certs = certs(NAME);
        let mut w = world(&certs, 1);
        let (c, s) = w.pair(Addr::Ip(sim_addr()), prefix().with_rx(rx));
        // Closed outright: no close alert goes out.
        let (client, io) = w.client();
        client.close(io, c).unwrap();
        let mut ended = None;
        w.until(|_, server, e| {
            match e {
                Event::Closed { conn, why } if server && conn == s => ended = Some(why),
                Event::Up(_)
                | Event::Accepted { .. }
                | Event::Refused { .. }
                | Event::Readable(_)
                | Event::Writable(_)
                | Event::Down { .. }
                | Event::Closed { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => panic!("{rx:?}: unexpected {e:?}"),
            }
            ended.is_some()
        });
        assert_eq!(ended, Some(Why::Reset), "{rx:?}");
    }
}

#[test]
fn a_record_that_found_no_room_in_frame_memory_is_opened_once_there_is_room() {
    let certs = certs(NAME);
    let mut w = world(&certs, 1);
    // Frame memory holds 32 KiB: two frames of a whole record each fill
    // it, and the second record's plaintext needs a byte more than is left
    // behind the first while that one has not been pulled.
    let framing = HeaderFraming::length_prefix(32 << 10).unwrap();
    let opts = ConnOpts::new(Framing::Header(framing));
    let (c, s) = w.pair(Addr::Ip(sim_addr()), opts);
    let frames = [check::prefixed(16_380, 1), check::prefixed(16_380, 2)];
    for frame in &frames {
        assert!(w.send(c, frame));
    }
    // Everything arrives before the server's caller pulls anything.
    for _ in 0..600 {
        w.turn();
        let (client, _) = w.client();
        assert_eq!(client.next_event(), None);
    }
    let (server, _) = w.server();
    assert_eq!(server.next_event(), Some(Event::Readable(s)));
    let got: Vec<Vec<u8>> = w.pull_all(s).into_iter().map(|(_, bytes)| bytes).collect();
    assert!(got == frames, "{} of 2 frames were pulled", got.len());
}

#[test]
fn records_opened_when_frame_memory_returns_are_announced() {
    let certs = certs(NAME);
    let mut w = world(&certs, 1);
    let (c, s) = w.pair(Addr::Ip(sim_addr()), prefix());
    let frame = check::prefixed(100, 3);
    assert!(w.send(c, &frame));
    // Every time bytes arrive all frame memory is in use elsewhere, and
    // it is free again before the set is next polled.
    let mut elsewhere = Vec::new();
    let mut readable = false;
    for _ in 0..200 {
        let now = w.io().now_ns();
        let (server, io) = w.server();
        for ext in elsewhere.drain(..) {
            assert_eq!(server.extents().release(ext), Ok(true));
        }
        server.poll(io, now);
        elsewhere.extend(std::iter::from_fn(|| server.extents().take()));
        w.turn();
        let (client, _) = w.client();
        assert_eq!(client.next_event(), None);
        if let Some(e) = w.server().0.next_event() {
            assert_eq!(e, Event::Readable(s));
            readable = true;
            break;
        }
    }
    assert!(readable, "the frame was opened and never announced");
    let got: Vec<Vec<u8>> = w.pull_all(s).into_iter().map(|(_, bytes)| bytes).collect();
    assert_eq!(got, [frame]);
}

#[test]
fn tls_is_refused_where_it_was_not_sized_or_set_up() {
    let mut w = World::usual(&mut |l| SimIo::new(l, 1, Faults::NONE));
    let (client, io) = w.client();
    let asked = client.connect_tls(io, sim_addr(), NAME, prefix());
    assert_eq!(asked, Err(NetError::NoTls));
    // Sized and set up, but not with a name a certificate could carry.
    let certs = certs(NAME);
    let mut tls = world(&certs, 1);
    let (sized, sized_io) = tls.client();
    let unnamed = sized.connect_tls(sized_io, sim_addr(), "", prefix());
    assert_eq!(unnamed, Err(NetError::NoTls));
}
