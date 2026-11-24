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
    ListenConfig, Listener, Net, SendError, ServerName, Sizes,
};
use core::fmt;
use longshore_common::io::turn::{Class, Turn};
use longshore_common::io::{Completion, Completions, Io, Limits, Op, Token};
use std::collections::VecDeque;

mod exchange;
pub mod storage;

pub use exchange::run_all;
#[cfg(feature = "tls-core")]
pub use exchange::run_all_tls;
#[cfg(feature = "tls-core")]
pub use secure::Certs;

#[cfg(feature = "tls-core")]
pub mod secure;

/// The harness's own timer, outside both sets' token ranges.
const TICK: Token = Token::new(1 << 40);
const TICK_NS: u64 = 250_000;
/// The first of the 64 tokens a check's file operations use.
pub const STORAGE: u64 = 1 << 41;

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
    /// Which completions are storage's and which the sets'.
    classes: Turn,
    /// Storage completions of the last turns, for the check to take.
    storage: VecDeque<Completion>,
    /// With TLS: the name the server's certificate carries.
    tls: Option<ServerName>,
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
        World::build(make, base, (client, server), None, |t, _| t)
    }

    /// The world, with each set's transport passed through `wrap` (`true`
    /// for the client's) before its `Net` is built.
    fn build(
        make: &mut impl FnMut(Limits) -> IO,
        base: Limits,
        (client, server): (Sizes, Sizes),
        tls: Option<ServerName>,
        wrap: impl Fn(KernelTransport<IO>, bool) -> KernelTransport<IO>,
    ) -> World<IO> {
        let (l1, ccfg) = must(Config::plan(base, client), "client plan");
        let after = server.with_token_base(ccfg.token_base() + ccfg.tokens());
        let (limits, scfg) = must(Config::plan(l1, after), "server plan");
        World {
            io: make(limits.with_ops_max(limits.ops_max() + 4)),
            client: Net::new(wrap(KernelTransport::new(&ccfg), true), ccfg, &limits),
            server: Net::new(wrap(KernelTransport::new(&scfg), false), scfg, &limits),
            out: Completions::new(64),
            sends: 0,
            ticking: false,
            classes: Turn::new(
                (STORAGE, STORAGE + 64),
                (ccfg.token_base(), scfg.token_base() + scfg.tokens()),
            ),
            storage: VecDeque::with_capacity(64),
            tls,
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
        // Storage completions first, as the owner of a log would take them.
        let World {
            io,
            client,
            server,
            out,
            sends,
            ticking,
            classes,
            storage,
            tls: _,
        } = self;
        classes.dispatch(out, |class, c| match class {
            Class::Storage => storage.push_back(c),
            Class::Network => {
                // Kind 1 of a connection's tokens is its send.
                *sends += u32::from(c.token().get() % 8 == 1);
                if !(client.on_completion(io, c) || server.on_completion(io, c)) {
                    fail(format_args!("a completion nobody claimed: {c:?}"));
                }
            }
            Class::Other if c.token() == TICK => *ticking = false,
            Class::Other => fail(format_args!("a completion nobody claimed: {c:?}")),
        });
    }

    /// The next storage completion of the turns so far.
    pub fn next_storage(&mut self) -> Option<Completion> {
        self.storage.pop_front()
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
        let cfg = cfg.with_tls(self.tls.is_some());
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
        let c = match self.tls {
            Some(name) => self
                .client
                .connect_tls(&mut self.io, addr, name.as_str(), opts),
            None => self.client.connect(&mut self.io, addr, opts),
        };
        let c = must(c, "connect");
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
