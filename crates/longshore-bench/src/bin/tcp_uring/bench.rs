// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The benchmarks of `tcp_uring`: a thread under test that owns the
//! connections it opened, and a peer thread that accepts them, each with
//! a backend and a connection set of its own, over loopback.
//!
//!   tcp_uring rr   [options]   request and response; latency
//!   tcp_uring bulk [options]   frames one way; bytes a second and CPU
//!   tcp_uring path [options]   a block logged, replicated and answered
//!
//!   --backend uring|epoll   the backend of both threads (uring)
//!   --secs N                how long to measure (3)
//!   --conns N               connections (1)
//!   --body N                rr: bytes behind the 256-byte header (64)
//!   --frame N               bulk: bytes a frame (65536)
//!   --rx ring|direct        receive mode of both ends (ring)
//!   --ring N                bytes a ring buffer (4096)
//!   --zc-min N              sends at least this long are zero-copy (16384)
//!   --segment N             bytes a send segment (65536)
//!   --wait sleep|spin       wait for completions, or turn without waiting
//!   --sync fsync|dsync      path: sync after each write, or open the log
//!                           so that a write is durable when it completes
//!   --dir PATH              path: where the log goes (a file system with
//!                           direct I/O; the temporary directory)
//!
//! `--wait` is for both threads. The time measured starts when every
//! connection is up, and ends before any is closed. The CPU figures are of
//! the two threads themselves: what the ring hands to its kernel workers
//! (a sync, a write that cannot complete at once) is not in them, while
//! the portable backend does the same work on the thread, so the CPU
//! columns of `path` do not compare across backends.
//!
//! One JSON line a run. GB is 10^9 bytes; times are of the request and its
//! answer, in microseconds.

use crate::die::{die, OrDie};
use longshore::io::{Completion, Completions, Io, Limits, Op, Token};
use longshore::tcp::{
    Config, Conn, ConnOpts, Event, Framing, HeaderFraming, KernelTransport, ListenConfig, Net, Rx,
    SendError, Sizes,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

mod load;
mod path;

/// What a run measures.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A request and its answer.
    Rr,
    /// Frames one way.
    Bulk,
    /// A block logged, replicated and answered.
    Path,
}

/// The block a log node writes: a frame of `path`, prefix included.
pub(crate) const BLOCK: u32 = 4096;

/// What a run is asked to be.
#[derive(Clone)]
pub(crate) struct Opts {
    pub(crate) kind: Kind,
    pub(crate) dsync: bool,
    pub(crate) dir: String,
    pub(crate) epoll: bool,
    pub(crate) secs: u64,
    pub(crate) conns: u32,
    pub(crate) body: u32,
    pub(crate) frame: u32,
    pub(crate) rx: Rx,
    pub(crate) ring: u32,
    pub(crate) zc_min: u32,
    pub(crate) segment: u32,
    pub(crate) spin: bool,
}

impl Opts {
    /// The length of every frame of the run, its four-byte prefix included.
    pub(crate) const fn frame_len(&self) -> u32 {
        match self.kind {
            Kind::Rr => 4 + HEADER + self.body,
            Kind::Bulk => self.frame,
            Kind::Path => BLOCK,
        }
    }

    fn parse(cmd: &str, mut args: impl Iterator<Item = String>) -> Opts {
        let kind = match cmd {
            "bulk" => Kind::Bulk,
            "path" => Kind::Path,
            _ => Kind::Rr,
        };
        let mut o = Opts {
            kind,
            dsync: false,
            dir: std::env::temp_dir().to_string_lossy().into_owned(),
            epoll: false,
            secs: 3,
            conns: 1,
            body: 64,
            frame: 65_536,
            rx: Rx::Ring,
            ring: 4096,
            zc_min: 16_384,
            segment: 65_536,
            spin: false,
        };
        while let Some(flag) = args.next() {
            let value = args.next().or_die(&format!("{flag} needs a value"));
            let number = || {
                value
                    .parse::<u32>()
                    .ok()
                    .or_die(&format!("{flag} takes a number"))
            };
            match flag.as_str() {
                "--backend" => o.epoll = pick(&flag, &value, "uring", "epoll"),
                "--secs" => o.secs = u64::from(number()),
                "--conns" => o.conns = number().max(1),
                "--body" => o.body = number(),
                "--frame" => o.frame = number().max(8),
                "--rx" if pick(&flag, &value, "ring", "direct") => o.rx = Rx::Direct,
                "--rx" => o.rx = Rx::Ring,
                "--ring" => o.ring = number().max(64),
                "--zc-min" => o.zc_min = number(),
                "--segment" => o.segment = number(),
                "--wait" => o.spin = pick(&flag, &value, "sleep", "spin"),
                "--sync" => o.dsync = pick(&flag, &value, "fsync", "dsync"),
                "--dir" => o.dir.clone_from(&value),
                other => die(format_args!("unknown flag {other}")),
            }
        }
        if o.kind == Kind::Path && o.conns > 500 {
            // A replica is sent every source's block; its queue of sends
            // holds two for each, up to the thousand a connection can.
            die("path takes up to 500 connections");
        }
        o
    }

    /// Connections of a run: with `path`, three replicas beside the
    /// sources.
    pub(crate) fn lines(&self) -> u32 {
        self.conns + if self.kind == Kind::Path { 3 } else { 0 }
    }

    /// Sends a connection can have queued. A replica of a log node is
    /// sent every source's block, all of them at once when their writes
    /// complete together: its queue holds two for each source.
    fn spans(&self) -> u32 {
        if self.kind == Kind::Path {
            (2 * self.conns + 8).clamp(16, 1024)
        } else {
            16
        }
    }

    /// The pools of one end: `connects` it opens, `accepts` it takes.
    fn sizes(&self, connects: u32, accepts: u32) -> Sizes {
        let frame = self.frame_len();
        let many = (2 * self.lines()).clamp(16, 8192);
        Sizes::new(connects, accepts, u32::from(accepts > 0))
            .with_frames(many, frame.next_multiple_of(4096))
            .with_segments(many, self.segment.max(frame))
            .with_ring((4 * self.lines()).clamp(64, 16_384), self.ring)
            .with_spans(self.spans())
            .with_zero_copy_min(self.zc_min)
    }

    pub(crate) fn conn_opts(&self) -> ConnOpts {
        let framing = HeaderFraming::length_prefix(self.frame_len())
            .ok()
            .or_die("framing");
        ConnOpts::new(Framing::Header(framing)).with_rx(self.rx)
    }

    /// The options of the peer's ends. A log node receives each block at
    /// the start of frame memory of its own, to write it from there.
    fn peer_opts(&self) -> ConnOpts {
        if self.kind == Kind::Path {
            self.conn_opts().with_rx(Rx::Direct)
        } else {
            self.conn_opts()
        }
    }
}

/// Whether `value` is `yes` rather than `no`; anything else is refused.
fn pick(flag: &str, value: &str, no: &str, yes: &str) -> bool {
    if value != no && value != yes {
        die(format_args!("{flag} takes {no} or {yes}"));
    }
    value == yes
}

/// The header a request carries behind its length prefix, as UltraGraphDB's.
pub(crate) const HEADER: u32 = 256;
/// The timer that wakes a thread that waits with nothing else due.
const BEAT: Token = Token::new(1 << 40);

/// A backend and a connection set on it: one thread's.
pub(crate) struct Side<IO: Io> {
    pub(crate) io: IO,
    pub(crate) net: Net<KernelTransport<IO>>,
    out: Completions,
    /// Completions that are not the set's: a log node's file operations.
    pub(crate) others: std::collections::VecDeque<Completion>,
    beating: bool,
    spin: bool,
}

impl<IO: Io> Side<IO> {
    /// A side with room for a file and `files` file operations at once
    /// beside the set's own, if `files` is not zero.
    fn new(make: impl FnOnce(Limits) -> IO, sizes: Sizes, spin: bool, files: u32) -> Side<IO> {
        let base = Limits::BASE.with_files_max(u32::from(files > 0));
        let (limits, config) = Config::plan(base, sizes).ok().or_die("plan");
        let io = make(limits.with_ops_max(limits.ops_max() + 2 + files));
        let net = Net::new(KernelTransport::new(&config), config, &limits);
        Side {
            io,
            net,
            out: Completions::new(1024),
            others: std::collections::VecDeque::with_capacity(1024),
            beating: false,
            spin,
        }
    }

    /// Poll the set, wait for completions (or not, when spinning) and
    /// hand them to the set.
    pub(crate) fn turn(&mut self) {
        let now = self.io.now_ns();
        self.net.poll(&mut self.io, now);
        if !self.spin && !self.beating {
            // So that a wait ends though nothing else is due.
            self.beating = self
                .io
                .submit(BEAT, Op::Timeout { nanos: 5_000_000 })
                .is_ok();
        }
        self.io.tick(!self.spin, &mut self.out).or_die("tick");
        while let Some(c) = self.out.pop() {
            if c.token() == BEAT {
                self.beating = false;
            } else if !self.net.on_completion(&mut self.io, c) {
                self.others.push_back(c);
            }
        }
    }
}

/// What the peer thread saw: frames and bytes received, and its CPU time.
#[derive(Clone, Copy, Default)]
pub(crate) struct Peer {
    pub(crate) frames: u64,
    pub(crate) bytes: u64,
    pub(crate) cpu: f64,
}

/// CPU time of the calling thread, in seconds.
pub(crate) fn thread_cpu() -> f64 {
    extern "C" {
        fn clock_gettime(clock: i32, ts: *mut [i64; 2]) -> i32;
    }
    let mut ts = [0i64; 2];
    // CLOCK_THREAD_CPUTIME_ID; a failure leaves zero, which reads as none.
    let _: i32 = unsafe { clock_gettime(3, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

/// Run the benchmark `cmd` names with the rest of the arguments.
pub(crate) fn run(cmd: &str, args: impl Iterator<Item = String>) {
    let o = Opts::parse(cmd, args);
    if o.epoll {
        measure(&o, || {
            |l| longshore::io::posix::PosixIo::new(l).or_die("PosixIo::new")
        });
    } else {
        measure(&o, || {
            |l| longshore::io::uring::UringIo::new(l).or_die("UringIo::new")
        });
    }
}

/// The peer on a thread of its own, then the load on this one.
fn measure<IO: Io, M: FnOnce(Limits) -> IO + Send + 'static>(o: &Opts, make: impl Fn() -> M) {
    let stop = Arc::new(AtomicBool::new(false));
    let (tell, told) = mpsc::channel();
    let (peer_make, peer_stop, at_peer) = (make(), Arc::clone(&stop), o.clone());
    let peer = std::thread::spawn(move || {
        let files = if at_peer.kind == Kind::Path {
            at_peer.lines()
        } else {
            0
        };
        let mut side = Side::new(
            peer_make,
            at_peer.sizes(0, at_peer.lines()),
            at_peer.spin,
            files,
        );
        let any = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        // Every connection of the run is opened at once.
        let how = ListenConfig::new(at_peer.peer_opts()).with_backlog(at_peer.lines().max(128));
        let listener = side
            .net
            .listen(&mut side.io, any, how)
            .ok()
            .or_die("listen");
        let addr = side
            .net
            .listener_addr(listener)
            .or_die("the listener's address");
        tell.send(addr).ok().or_die("the peer's address");
        if at_peer.kind == Kind::Path {
            path::serve(&mut side, &peer_stop, &at_peer)
        } else {
            load::serve(&mut side, &peer_stop)
        }
    });
    let addr = told
        .recv_timeout(Duration::from_secs(10))
        .ok()
        .or_die("the peer");
    let mut side = Side::new(make(), o.sizes(o.lines(), 0), o.spin, 0);
    let done = load::drive(&mut side, o, addr);
    stop.store(true, Ordering::Release);
    let seen = peer.join().ok().or_die("the peer thread");
    load::report(o, &done, &seen);
}

/// The end of a send that could not be queued: full, or a fault.
pub(crate) fn queued(sent: Result<(), SendError>, conn: Conn) -> bool {
    match sent {
        Ok(()) => true,
        Err(SendError::Full) => false,
        Err(e) => die(format_args!("send on {conn:?}: {e:?}")),
    }
}

/// Events that end a benchmark: nothing here closes or fails in order.
pub(crate) fn unexpected(e: Event) -> ! {
    die(format_args!("unexpected {e:?}"))
}
