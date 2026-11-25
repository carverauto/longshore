// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TCP send coalescing microbenchmarks: small frames queued in one turn
//! share a send segment and leave in fewer sends; the same frames queued
//! one per turn each leave in a send of their own. Both run on the
//! simulator, so a figure is the whole way of a frame through both
//! connection sets and the simulated network, not the cost of a system call.
//!
//!   cargo bench -p longshore-bench --bench tcp_coalescing
//!   cargo bench -p longshore-bench --bench tcp_coalescing -- --save-baseline main

use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, Throughput};
use longshore::io::Limits;
use longshore::sim::{Faults, SimIo};
use longshore::tcp::check::{prefixed, World};
use longshore::tcp::{Addr, Conn, ConnOpts, Framing, HeaderFraming, ListenConfig, Listener, Sizes};
use std::hint::black_box;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Frames in one measured round. With the largest body they still fit one
/// send segment.
const BATCH: u32 = 8;
/// Bytes in a send segment, a ring buffer and a buffer of frame memory.
const BUFFER: u32 = 16 << 10;
/// Turns after which a round that has not finished is given up.
const TURNS_MAX: u32 = 10_000;

/// Stop the benchmark with `why`.
fn stop(why: &str) -> ! {
    eprintln!("tcp_coalescing: {why}");
    std::process::exit(1)
}

/// One connection on the simulator, without faults: the world, the
/// client's end and the server's.
fn link() -> (World<SimIo>, Conn, Conn) {
    let Ok(framing) = HeaderFraming::length_prefix(BUFFER) else {
        stop("the length prefix is not a usable framing")
    };
    let opts = ConnOpts::new(Framing::Header(framing));
    let sizes = |connects, accepts, listeners| {
        Sizes::new(connects, accepts, listeners)
            .with_frames(16, BUFFER)
            .with_segments(16, BUFFER)
            .with_ring(16, BUFFER)
    };
    let mut w = World::new(
        &mut |l| SimIo::new(l, 9, Faults::NONE),
        Limits::BASE,
        sizes(1, 0, 0),
        sizes(0, 1, 1),
    );
    let addr = Addr::Ip(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)),
        7000,
    ));
    let _: Listener = w.listen(addr, ListenConfig::new(opts));
    let (c, s) = w.connect(addr, opts);
    (w, c, s)
}

/// One turn of the loop, then what it brought: events are dropped, and
/// the frames on `s` are released and counted, so that no pool fills.
fn turn(w: &mut World<SimIo>, s: Conn) -> u32 {
    w.turn();
    while w.client().0.next_event().is_some() {}
    while w.server().0.next_event().is_some() {}
    let mut got = 0;
    loop {
        let (net, io) = w.server();
        match net.next_frame(io, s) {
            Ok(Some(frame)) => {
                net.release(frame);
                got += 1;
            }
            Ok(None) => return got,
            Err(_) => stop("the server's end is gone"),
        }
    }
}

/// Queue `frame` on `c`, turning while there is no room; the frames that
/// arrived on `s` meanwhile.
fn queue(w: &mut World<SimIo>, (c, s): (Conn, Conn), frame: &[u8]) -> u32 {
    let mut got = 0;
    for _ in 0..TURNS_MAX {
        if w.send(c, frame) {
            return got;
        }
        got += turn(w, s);
    }
    stop("never room to send")
}

/// Send `BATCH` copies of `frame` and turn until all have arrived: with a
/// turn after each when `apart`, otherwise all queued before the first turn.
fn round(w: &mut World<SimIo>, ends: (Conn, Conn), frame: &[u8], apart: bool) {
    let mut got = 0;
    for _ in 0..BATCH {
        got += queue(w, ends, frame);
        if apart {
            got += turn(w, ends.1);
        }
    }
    for _ in 0..TURNS_MAX {
        if got == BATCH {
            return;
        }
        got += turn(w, ends.1);
    }
    stop("the frames did not arrive")
}

/// The sends one round takes.
fn sends(w: &mut World<SimIo>, ends: (Conn, Conn), frame: &[u8], apart: bool) -> u32 {
    *w.sends() = 0;
    round(w, ends, frame, apart);
    *w.sends()
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("tcp_coalescing");
    let _: &mut BenchmarkGroup<'_, WallTime> = g.throughput(Throughput::Elements(BATCH.into()));
    let (mut w, client, server) = link();
    let ends = (client, server);
    for body in [64usize, 512, 1500] {
        let frame = prefixed(body, 7);
        // Queued together the frames must share sends, or nothing is compared.
        let together = sends(&mut w, ends, &frame, false);
        let apart = sends(&mut w, ends, &frame, true);
        assert!(
            together < apart && apart >= BATCH,
            "{body} bytes: {together} sends together, {apart} apart"
        );
        for (mode, one_per_turn) in [("together", false), ("apart", true)] {
            let _: &mut BenchmarkGroup<'_, WallTime> = g
                .bench_function(format!("{mode}/{body}_bytes"), |b| {
                    b.iter(|| round(&mut w, ends, black_box(&frame), one_per_turn))
                });
        }
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
