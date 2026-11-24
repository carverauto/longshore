// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Checks of `Net` that ship in the library, so that every backend runs the
//! same ones: the simulator and the portable backend from this crate's
//! tests, the ring from `longshore-bench` on a real kernel.
//!
//! [`World`] is a client set and a server set on one backend, as two
//! components share one ring, with the owner's loop around them.
//! [`run_all`] runs the checks and returns every frame the servers
//! received, in order: the same on every backend.

use crate::{
    Addr, Config, Conn, ConnOpts, Event, Frame, Framing, HeaderFraming, KernelTransport,
    ListenConfig, Listener, Net, Rx, SendError, Sizes, Why,
};
use core::fmt;
use longshore_common::io::{Completions, Io, Limits, Op, Token};
use std::net::SocketAddr;

/// The harness's own timer, outside both sets' token ranges.
const TICK: Token = Token::new(1 << 40);
const TICK_NS: u64 = 250_000;

/// Stop the running check with `why`.
#[track_caller]
#[expect(
    clippy::panic,
    reason = "a check is a test: stopping it with a message is how it fails"
)]
pub fn fail(why: fmt::Arguments<'_>) -> ! {
    panic!("{why}")
}

/// The value a check needs, or fail naming `what`.
#[track_caller]
pub fn must<V, E: fmt::Debug>(value: Result<V, E>, what: &str) -> V {
    match value {
        Ok(v) => v,
        Err(e) => fail(format_args!("{what}: {e:?}")),
    }
}

/// A client set and a server set on one backend.
pub struct World<IO: Io> {
    io: IO,
    client: Net<KernelTransport<IO>>,
    server: Net<KernelTransport<IO>>,
    out: Completions,
    sends: u32,
    ticking: bool,
}

/// Small sizes, so that frames span ring buffers and queues fill.
pub fn sizes(connects: u32, accepts: u32, listeners: u32) -> Sizes {
    Sizes::new(connects, accepts, listeners)
        .with_frames(8, 8192)
        .with_segments(8, 4096)
        .with_ring(4, 1024)
        .with_header_max(256)
        .with_spans(4)
}

/// A frame with a four-byte length prefix and `len` patterned bytes.
pub fn prefixed(len: usize, seed: u8) -> Vec<u8> {
    let mut f = (len as u32).to_be_bytes().to_vec();
    f.extend((0..len).map(|i| (i as u8).wrapping_mul(31) ^ seed));
    f
}

/// Frames with a four-byte length prefix, up to 8192 bytes in all.
pub fn prefix() -> ConnOpts {
    let framing = must(HeaderFraming::length_prefix(8192), "framing");
    ConnOpts::new(Framing::Header(framing))
}

impl<IO: Io> World<IO> {
    /// A world on a backend that `make` builds from the `Limits` both sets
    /// need on top of `base`. The server's tokens follow the client's.
    pub fn new(
        make: &mut impl FnMut(Limits) -> IO,
        base: Limits,
        client: Sizes,
        server: Sizes,
    ) -> World<IO> {
        let (l1, ccfg) = must(Config::plan(base, client), "client plan");
        let after = server.with_token_base(ccfg.token_base() + ccfg.tokens());
        let (limits, scfg) = must(Config::plan(l1, after), "server plan");
        World {
            io: make(limits.with_ops_max(limits.ops_max() + 4)),
            client: Net::new(KernelTransport::new(&ccfg), ccfg, &limits),
            server: Net::new(KernelTransport::new(&scfg), scfg, &limits),
            out: Completions::new(64),
            sends: 0,
            ticking: false,
        }
    }

    /// The usual world: four connections out, four in, two listeners.
    pub fn usual(make: &mut impl FnMut(Limits) -> IO) -> World<IO> {
        World::new(make, Limits::BASE, sizes(4, 0, 0), sizes(2, 4, 2))
    }

    /// The usual world, about to listen on `addr`: a socket file an
    /// earlier world left at a Unix path is removed first.
    pub fn listening_on(make: &mut impl FnMut(Limits) -> IO, addr: &Addr) -> World<IO> {
        if let Addr::Unix(path) = addr {
            // Usually there is none.
            drop(std::fs::remove_file(path.as_str()));
        }
        World::usual(make)
    }

    /// The backend.
    pub fn io(&mut self) -> &mut IO {
        &mut self.io
    }

    /// The client set and the backend.
    pub fn client(&mut self) -> (&mut Net<KernelTransport<IO>>, &mut IO) {
        (&mut self.client, &mut self.io)
    }

    /// The server set and the backend.
    pub fn server(&mut self) -> (&mut Net<KernelTransport<IO>>, &mut IO) {
        (&mut self.server, &mut self.io)
    }

    /// Sends that have completed so far; settable, to count from a point.
    pub fn sends(&mut self) -> &mut u32 {
        &mut self.sends
    }

    /// One turn of the owner's loop. The caller has pulled the events and
    /// frames of the turn before: poll both sets, then collect and route.
    pub fn turn(&mut self) {
        let now = self.io.now_ns();
        self.client.poll(&mut self.io, now);
        self.server.poll(&mut self.io, now);
        if !self.ticking {
            // Something to wait for, so that time passes when nothing else does.
            must(
                self.io.submit(TICK, Op::Timeout { nanos: TICK_NS }),
                "timer",
            );
            self.ticking = true;
        }
        must(self.io.tick(true, &mut self.out), "tick");
        while let Some(c) = self.out.pop() {
            if c.token() == TICK {
                self.ticking = false;
                continue;
            }
            // Kind 1 of a connection's tokens is its send.
            self.sends += u32::from(c.token().get() % 8 == 1);
            let taken = self.client.on_completion(&mut self.io, c)
                || self.server.on_completion(&mut self.io, c);
            if !taken {
                fail(format_args!("a completion nobody claimed: {c:?}"));
            }
        }
    }

    /// Turn until `done` says so, handing it each event with the side
    /// (`true` for the server) it came from.
    pub fn until(&mut self, mut done: impl FnMut(&mut World<IO>, bool, Event) -> bool) {
        for _ in 0..40_000 {
            while let Some(e) = self.client.next_event() {
                if done(self, false, e) {
                    return;
                }
            }
            while let Some(e) = self.server.next_event() {
                if done(self, true, e) {
                    return;
                }
            }
            self.turn();
        }
        fail(format_args!("the check did not finish"));
    }

    /// A listener on the server.
    pub fn listen(&mut self, addr: Addr, cfg: ListenConfig) -> Listener {
        must(self.server.listen(&mut self.io, addr, cfg), "listen")
    }

    /// Where to dial `listener`: the address it is bound to (which names
    /// the port when `addr` left it open), or `addr` itself for a path.
    pub fn dial_addr(&self, listener: Listener, addr: Addr) -> Addr {
        self.server.listener_addr(listener).map_or(addr, Addr::Ip)
    }

    /// A listener on `addr` and one connection to it, both ends up.
    pub fn pair(&mut self, addr: Addr, opts: ConnOpts) -> (Conn, Conn) {
        let l = self.listen(addr, ListenConfig::new(opts));
        let to = self.dial_addr(l, addr);
        self.connect(to, opts)
    }

    /// One more connection to `addr`, both ends up.
    pub fn connect(&mut self, addr: Addr, opts: ConnOpts) -> (Conn, Conn) {
        let c = must(self.client.connect(&mut self.io, addr, opts), "connect");
        let (mut up, mut accepted) = (false, None);
        self.until(|_, _, e| {
            match e {
                Event::Up(conn) => up |= conn == c,
                Event::Accepted { conn, .. } => accepted = Some(conn),
                Event::Refused { .. }
                | Event::Readable(_)
                | Event::Writable(_)
                | Event::Down { .. }
                | Event::Closed { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => fail(format_args!("connecting: unexpected {e:?}")),
            }
            up && accepted.is_some()
        });
        match accepted {
            Some(s) => (c, s),
            None => fail(format_args!("no accepted connection")),
        }
    }

    /// Pull every frame waiting on `conn` of the server, copying each out.
    pub fn pull_all(&mut self, conn: Conn) -> Vec<(Frame, Vec<u8>)> {
        let mut got = Vec::new();
        while let Some(f) = must(self.server.next_frame(&mut self.io, conn), "next_frame") {
            match self.server.bytes(&mut self.io, &f) {
                Some(bytes) => got.push((f, bytes.to_vec())),
                None => fail(format_args!("a frame just pulled has no bytes")),
            }
        }
        got
    }

    /// Queue `frame` on `conn` of the client; `false` when the queue is full.
    pub fn send(&mut self, conn: Conn, frame: &[u8]) -> bool {
        match self.client.send_bytes(&mut self.io, conn, frame) {
            Ok(()) => true,
            Err(SendError::Full) => false,
            Err(e) => fail(format_args!("send: {e:?}")),
        }
    }

    /// Send `frames` from client `c` and return what server `s` receives,
    /// releasing each frame as it is taken.
    pub fn exchange(&mut self, c: Conn, s: Conn, frames: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let (mut next, mut got) = (0, Vec::new());
        let total: usize = frames.iter().map(Vec::len).sum();
        let mut bytes = 0;
        for _ in 0..40_000 {
            if bytes == total && next == frames.len() {
                return got;
            }
            while next < frames.len() && self.send(c, &frames[next]) {
                next += 1;
            }
            self.turn();
            while self.client.next_event().is_some() {}
            while let Some(e) = self.server.next_event() {
                if e != Event::Readable(s) {
                    fail(format_args!("exchange: unexpected {e:?}"));
                }
                for (f, data) in self.pull_all(s) {
                    bytes += data.len();
                    got.push(data);
                    self.server.release(f);
                }
            }
        }
        fail(format_args!("exchange: {bytes} of {total} bytes arrived"))
    }
}

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
