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
//!   --zc-sends N            zero-copy sends a connection may have with their
//!                           memory still out, 256 in all; 0: the next send
//!                           waits for the last one's memory (4)
//!   --segment N             bytes a send segment (65536)
//!   --wait sleep|spin       wait for completions, or turn without waiting
//!   --sync fsync|dsync      path: sync after each write, or open the log
//!                           so that a write is durable when it completes
//!   --dir PATH              path: where the log goes (a file system with
//!                           direct I/O; the temporary directory)
//!   --header N              path: a block is the body of a frame, behind
//!                           a header of N bytes with the frame's length
//!                           at 40, as UltraGraphDB's (0: a block is the
//!                           whole frame, behind a four-byte prefix)
//!   --body-at block|frame   path, with a header: the node receives a
//!                           frame so that its body lies at a block
//!                           boundary and writes it from there, or
//!                           receives it at the start of its memory and
//!                           moves the body to a block before the write
//!                           (frame)
//!
//!   --listen ADDR:PORT      be the peer alone, at this address, until every
//!                           connection of the run has come and closed
//!   --connect ADDR:PORT     be the load alone, against a peer elsewhere
//!
//! With `--listen` and `--connect` the two ends run on two machines, with
//! the same options otherwise: the peer first, and a peer for every run.
//! A peer whose load has not come and gone a minute after the run's time
//! ends as failed. Each prints a line of its
//! own; the load's has no figures of the peer, and counts as carried what
//! it sent.
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
use longshore::tcp::{Config, Conn, Event, KernelTransport, ListenConfig, Net, SendError, Sizes};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

mod load;
mod opts;
mod path;
mod stop;

pub(crate) use opts::Opts;
pub(crate) use stop::Stop;

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

/// The block a log node writes: a frame of `path`, prefix included, or
/// the body behind its header.
pub(crate) const BLOCK: u32 = 4096;

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
    /// From the first frame to the stop.
    pub(crate) secs: f64,
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

/// The peer's end: a set that listens at `at`, and where it does.
fn listening<IO: Io>(
    o: &Opts,
    make: impl FnOnce(Limits) -> IO,
    at: SocketAddr,
) -> (Side<IO>, SocketAddr) {
    let files = if o.kind == Kind::Path { o.lines() } else { 0 };
    let mut side = Side::new(make, o.sizes(0, o.lines()), o.spin, files);
    // Every connection of the run is opened at once.
    let how = ListenConfig::new(o.peer_opts()).with_backlog(o.lines().max(128));
    let listener = side.net.listen(&mut side.io, at, how).ok().or_die("listen");
    let addr = side
        .net
        .listener_addr(listener)
        .or_die("the listener's address");
    (side, addr)
}

/// Be the peer of the run until `stop` says so.
fn serve<IO: Io>(side: &mut Side<IO>, stop: &Stop, o: &Opts) -> Peer {
    if o.kind == Kind::Path {
        path::serve(side, stop, o)
    } else {
        load::serve(side, stop)
    }
}

/// The peer on a thread of its own, then the load on this one; or one of
/// the two alone, with the other on another machine.
fn measure<IO: Io, M: FnOnce(Limits) -> IO + Send + 'static>(o: &Opts, make: impl Fn() -> M) {
    if let Some(at) = o.listen {
        let (mut side, addr) = listening(o, make(), at);
        eprintln!("tcp_uring: listening at {addr}");
        // The load has a minute to start and to end beside its run.
        let patience = Duration::from_secs(o.secs + 60);
        let seen = serve(&mut side, &Stop::after_within(o.lines(), patience), o);
        return load::report_peer(o, &seen);
    }
    if let Some(addr) = o.connect {
        let mut side = Side::new(make(), o.sizes(o.lines(), 0), o.spin, 0);
        let done = load::drive(&mut side, o, addr);
        return load::report(o, &done, &Peer::default());
    }
    let stop = Arc::new(Stop::after(0));
    let (tell, told) = mpsc::channel();
    let (peer_make, peer_stop, at_peer) = (make(), Arc::clone(&stop), o.clone());
    let peer = std::thread::spawn(move || {
        let any = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let (mut side, addr) = listening(&at_peer, peer_make, any);
        tell.send(addr).ok().or_die("the peer's address");
        serve(&mut side, &peer_stop, &at_peer)
    });
    let addr = told
        .recv_timeout(Duration::from_secs(10))
        .ok()
        .or_die("the peer");
    let mut side = Side::new(make(), o.sizes(o.lines(), 0), o.spin, 0);
    let done = load::drive(&mut side, o, addr);
    stop.tell();
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
