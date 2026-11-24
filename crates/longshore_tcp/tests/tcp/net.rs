// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `Net` in the simulator: a client set and a server set on one backend,
//! as two components share one ring.

use longshore_common::io::{Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::{
    Config, Conn, ConnOpts, Event, Frame, Framing, HeaderFraming, KernelTransport, ListenConfig,
    Listener, Net, Rx, SendError, Sizes, Why,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(test)]
#[path = "net/accepting.rs"]
mod accepting;
#[cfg(test)]
#[path = "net/ending.rs"]
mod ending;
#[cfg(test)]
#[path = "net/frames.rs"]
mod frames;
#[cfg(test)]
#[path = "net/half_close.rs"]
mod half_close;
#[cfg(test)]
#[path = "net/input.rs"]
mod input;
#[cfg(test)]
#[path = "net/lifecycle.rs"]
mod lifecycle;
#[cfg(test)]
#[path = "net/listening.rs"]
mod listening;
#[cfg(test)]
#[path = "net/slots.rs"]
mod slots;

#[global_allocator]
static ALLOC: longshore_common::io::alloc_guard::Guard = longshore_common::io::alloc_guard::Guard;

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)), 7000);
const MS: u64 = 1_000_000;
/// A timer of the harness's own, outside both sets' token ranges.
const TICK: Token = Token::new(1 << 40);
/// A socket slot neither set uses.
const SPARE: Sock = Sock::new(0);

type SimNet = Net<KernelTransport<SimIo>>;

struct World {
    io: SimIo,
    client: SimNet,
    server: SimNet,
    out: Completions,
    /// Sends completed, for counting how many it took.
    sends: u32,
    ticking: bool,
}

#[cfg(test)]
fn sizes(connects: u32, accepts: u32, listeners: u32) -> Sizes {
    Sizes::new(connects, accepts, listeners)
        .with_frames(8, 8192)
        .with_segments(8, 4096)
        .with_ring(4, 1024)
        .with_header_max(256)
        .with_spans(4)
}

#[cfg(test)]
impl World {
    fn new(seed: u64, faults: Faults) -> World {
        World::with(seed, faults, sizes(4, 0, 0), sizes(2, 4, 2))
    }

    fn with(seed: u64, faults: Faults, client: Sizes, server: Sizes) -> World {
        let base = Limits::BASE.with_sockets_max(1).with_ops_max(8);
        let (l1, ccfg) = Config::plan(base, client).unwrap();
        let server = server.with_token_base(ccfg.token_base() + ccfg.tokens());
        let (limits, scfg) = Config::plan(l1, server).unwrap();
        World {
            io: SimIo::new(limits, seed, faults),
            client: Net::new(KernelTransport::new(&ccfg), ccfg, &limits),
            server: Net::new(KernelTransport::new(&scfg), scfg, &limits),
            out: Completions::new(64),
            sends: 0,
            ticking: false,
        }
    }

    /// One turn of the owner's loop. The caller has pulled the events and
    /// frames of the turn before: poll both sets, then collect and route.
    fn turn(&mut self) {
        let now = self.io.now_ns();
        self.client.poll(&mut self.io, now);
        self.server.poll(&mut self.io, now);
        if !self.ticking {
            // Virtual time moves only when something is waited for.
            self.io.submit(TICK, Op::Timeout { nanos: MS / 4 }).unwrap();
            self.ticking = true;
        }
        self.io.tick(true, &mut self.out).unwrap();
        while let Some(c) = self.out.pop() {
            if c.token() == TICK {
                self.ticking = false;
                continue;
            }
            // Kind 1 of a connection's tokens is its send.
            self.sends += u32::from(c.token().get() % 8 == 1);
            let taken = self.client.on_completion(&mut self.io, c)
                || self.server.on_completion(&mut self.io, c);
            assert!(taken, "a completion nobody claimed: {c:?}");
        }
    }

    /// Turn until `done` says so, handing each event to it with the side
    /// (`true` for the server) it came from.
    fn until(&mut self, mut done: impl FnMut(&mut World, bool, Event) -> bool) {
        for _ in 0..20_000 {
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
        panic!("the test did not finish");
    }

    fn listen(&mut self, cfg: ListenConfig) -> Listener {
        self.server.listen(&mut self.io, ADDR, cfg).unwrap()
    }

    /// A listener and one connection to it, both ends up.
    fn pair(&mut self, opts: ConnOpts) -> (Conn, Conn) {
        let _: Listener = self.listen(ListenConfig::new(opts));
        self.connect(opts)
    }

    /// One more connection to the listener, both ends up.
    fn connect(&mut self, opts: ConnOpts) -> (Conn, Conn) {
        let c = self.client.connect(&mut self.io, ADDR, opts).unwrap();
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
                | Event::Finished(_)
                | Event::Drained => panic!("unexpected {e:?}"),
            }
            up && accepted.is_some()
        });
        (c, accepted.unwrap())
    }

    /// Pull every frame waiting on `conn` of the server, copying each out.
    fn pull_all(&mut self, conn: Conn) -> Vec<(Frame, Vec<u8>)> {
        let mut got = Vec::new();
        while let Some(f) = self.server.next_frame(&mut self.io, conn).unwrap() {
            let bytes = self.server.bytes(&mut self.io, &f).unwrap().to_vec();
            got.push((f, bytes));
        }
        got
    }
}

/// A frame with a four-byte length prefix and `len` patterned bytes.
#[cfg(test)]
fn prefixed(len: usize, seed: u8) -> Vec<u8> {
    let mut f = (len as u32).to_be_bytes().to_vec();
    f.extend((0..len).map(|i| (i as u8).wrapping_mul(31) ^ seed));
    f
}

#[cfg(test)]
fn prefix() -> ConnOpts {
    ConnOpts::new(Framing::Header(HeaderFraming::length_prefix(8192).unwrap()))
}

/// Queue `frame` on `conn` of the client; `false` when the queue is full.
#[cfg(test)]
fn send(w: &mut World, conn: Conn, frame: &[u8]) -> bool {
    let r = w
        .client
        .send_frame(&mut w.io, conn, frame.len() as u32, |b| {
            b.copy_from_slice(frame)
        });
    match r {
        Ok(()) => true,
        Err(SendError::Full) => false,
        Err(e) => panic!("send: {e:?}"),
    }
}
