// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! I/O layer for the object storage client.
//!
//! Every module above this one is generic over [`Io`]. Production uses
//! `uring::UringIo`; tests use the deterministic simulator in `crate::sim`.
//!
//! Buffers live in a pool owned by the `Io` implementation and are named by
//! [`BufId`]. An in-flight operation owns its buffer until its completion is
//! delivered; for zero-copy sends that is after the kernel has released it.

use std::io;
use std::net::SocketAddr;

pub mod alloc_guard;
mod completion;
pub mod contract;
pub mod contract_chains;
pub mod contract_files;
pub mod contract_net;
pub mod engine;
pub mod extents;
pub use completion::{Completion, Completions};
pub mod files;
mod ids;
pub use files::{file_size, FileSlot, OpenFlags};
pub use ids::{BufId, FileId, Sock, Token};
pub mod limits;
mod must;
pub mod turn;
pub use limits::Limits;
#[cfg(unix)]
pub mod posix;
pub mod scenario;
pub mod segment;
pub use segment::Segment;
#[cfg(target_os = "linux")]
pub mod ring;
#[cfg(target_os = "linux")]
#[expect(
    clippy::exhaustive_structs,
    reason = "kernel ABI mirrors: the layout and every field are the kernel's, and tcp_raw builds raw sqes"
)]
pub mod sys;
#[cfg(target_os = "linux")]
pub mod uring;

/// Keepalive probe timing for TCP sockets: see [`Io::keep_alive_timing`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeepAlive {
    pub(crate) idle_secs: u32,
    pub(crate) interval_secs: u32,
    pub(crate) count: u32,
}

impl KeepAlive {
    /// Probe after `idle_secs` of silence, then every `interval_secs`, and
    /// give the connection up after `count` unanswered probes. Each at least 1.
    #[must_use]
    pub const fn new(idle_secs: u32, interval_secs: u32, count: u32) -> KeepAlive {
        assert!(idle_secs > 0 && interval_secs > 0 && count > 0);
        KeepAlive {
            idle_secs,
            interval_secs,
            count,
        }
    }
}

/// How [`Io::listen_with`] listens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ListenOpts {
    pub(crate) reuse_port: bool,
    pub(crate) backlog: u32,
}

impl ListenOpts {
    /// What [`Io::listen`] does: the port to itself, a backlog of 128.
    pub const DEFAULT: ListenOpts = ListenOpts {
        reuse_port: false,
        backlog: 128,
    };

    /// With `SO_REUSEPORT`: several sockets, each on its own thread and
    /// backend, listen on one address and the kernel spreads incoming
    /// connections across them.
    #[must_use]
    pub const fn with_reuse_port(mut self, on: bool) -> ListenOpts {
        self.reuse_port = on;
        self
    }

    /// With this many connections allowed to wait to be accepted (at least 1).
    #[must_use]
    pub const fn with_backlog(mut self, backlog: u32) -> ListenOpts {
        assert!(backlog > 0);
        self.backlog = backlog;
        self
    }
}

/// Which direction of a connection [`Op::Shutdown`] ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shut {
    /// No more receives: later receives complete with 0.
    Read,
    /// No more sends: the peer reads end of stream after the data sent.
    Write,
    /// Both directions.
    Both,
}

impl Shut {
    /// The `how` argument of `shutdown(2)`: the same on Linux and macOS.
    pub(crate) const fn raw(self) -> u32 {
        match self {
            Shut::Read => 0,
            Shut::Write => 1,
            Shut::Both => 2,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Open a TCP socket in slot `sock` and connect it. A connect that fails
    /// leaves the slot free; the caller need not close it. A connect that
    /// succeeds completes with 0, or with what [`Segment::decode`] reads
    /// when the backend was asked to measure segment sizes.
    Connect { sock: Sock, addr: SocketAddr },
    /// Open a Unix-domain stream socket in slot `sock` and connect it to the
    /// NUL-terminated path in `path_buf[path_off..path_off + path_len]`, copied
    /// at submission time. A connect that fails leaves the slot free.
    ConnectUnix {
        sock: Sock,
        path_buf: BufId,
        path_off: u32,
        path_len: u32,
    },
    /// Open a UDP socket in slot `sock` connected to `addr`. `Send` and `Recv`
    /// then carry whole datagrams.
    ConnectUdp { sock: Sock, addr: SocketAddr },
    /// Accept one connection from `listener` into slot `sock`.
    Accept { listener: Sock, sock: Sock },
    /// Accept connections on `listener`, placing each into a free slot of
    /// `Limits::accept_range_start..accept_range_start + accept_range_len` and
    /// reporting the slot index as `result` with `more: true`. Ends with
    /// `-ENFILE` or `-EMFILE` (`more: false`) when the range is full. Counts
    /// toward `Limits::ops_max` until its final completion.
    ArmedAccept { listener: Sock },
    /// Receive bytes on `sock`, taking a buffer from the registered buffer
    /// ring only when bytes arrive. Each completion with bytes carries
    /// `buf: Some(id)` and `result > 0` and remains owned by the caller until
    /// [`Io::give_back`]. Ends with `-ENOBUFS` when the ring is empty or `0`
    /// (`buf: None`) on peer EOF. Counts toward `Limits::ops_max` until its
    /// final completion.
    ArmedRecv { sock: Sock },
    /// Send `len` bytes of buffer `buf` starting at `off`. May complete short.
    Send {
        sock: Sock,
        buf: BufId,
        off: u32,
        len: u32,
        zero_copy: bool,
    },
    /// Receive up to `len` bytes into buffer `buf` at `off`. 0 means end of stream.
    Recv {
        sock: Sock,
        buf: BufId,
        off: u32,
        len: u32,
    },
    /// Like `Recv`, but never waits: completes with the bytes already
    /// received, 0 if the peer has closed, or `-EAGAIN` if nothing has
    /// arrived. Used to check an idle connection before it is reused.
    RecvNow {
        sock: Sock,
        buf: BufId,
        off: u32,
        len: u32,
    },
    /// Shut down one or both directions of connected stream socket `sock`,
    /// leaving its slot open. After the write side is shut down the peer
    /// reads end of stream once it has read everything sent; after the read
    /// side is shut down, receives on `sock` complete with 0. Completes with
    /// 0, or a negated errno (`-ENOTCONN` on a socket that is not connected).
    Shutdown { sock: Sock, how: Shut },
    /// Close socket slot `sock`. Gives every other operation in flight on
    /// `sock` (including an [`Op::Accept`] waiting on a closed listener) its
    /// final completion without waiting for the peer, in no fixed order
    /// relative to the close's own completion. Closing a slot that holds no
    /// socket still completes once.
    Close { sock: Sock },
    /// Wait until the backend's waker has been signalled from any thread.
    /// Completes with 0. Signals that arrive while no `AwaitWake` is in
    /// flight are kept, so the next one completes at once. Requires
    /// [`Io::waker`] to have been called.
    AwaitWake,
    /// Read a registered file from its start into buffer `buf` at `off`, up
    /// to `len` bytes. Completes with the number of bytes read; the file is
    /// opened and closed around the read, so a replaced file is seen.
    ReadFile {
        file: FileId,
        buf: BufId,
        off: u32,
        len: u32,
    },
    /// Completes with 0 after `nanos`.
    Timeout { nanos: u64 },
    /// Ask for the in-flight operation submitted with `target` at the moment
    /// this cancel is submitted to stop. An operation submitted later under
    /// the same token or already finished is not matched. The target then
    /// completes once, normally with `-ECANCELED`, or with its real result if
    /// it finished first. The cancel itself completes with 0 if found, or a
    /// negative errno if no such operation was in flight.
    Cancel { target: Token },
    /// Open the NUL-terminated path in `path_buf[path_off..path_off + path_len]`
    /// into fixed file slot `slot`. Completes with 0 or a negative errno.
    FileOpen {
        slot: FileSlot,
        path_buf: BufId,
        path_off: u32,
        path_len: u32,
        flags: OpenFlags,
    },
    /// Close the file in `slot`, freeing the slot on completion.
    FileClose { slot: FileSlot },
    /// Read up to `len` bytes at `file_off` from `slot` into `buf[off..off + len]`.
    FileRead {
        slot: FileSlot,
        buf: BufId,
        off: u32,
        len: u32,
        file_off: u64,
    },
    /// Write up to `len` bytes from `buf[off..off + len]` to `slot` at `file_off`.
    FileWrite {
        slot: FileSlot,
        buf: BufId,
        off: u32,
        len: u32,
        file_off: u64,
    },
    /// Flush dirty data (and metadata unless `datasync`) for `slot`.
    FileSync { slot: FileSlot, datasync: bool },
    /// Reserve space for `[file_off, file_off + len)` on `slot`.
    FileReserve {
        slot: FileSlot,
        file_off: u64,
        len: u64,
        keep_size: bool,
    },
    /// Deallocate `[file_off, file_off + len)` on `slot`, leaving file size unchanged.
    FilePunch {
        slot: FileSlot,
        file_off: u64,
        len: u64,
    },
    /// Read file status by path into `out_buf[out_off..]` in 256-byte Linux `statx` layout.
    FileStatus {
        path_buf: BufId,
        path_off: u32,
        path_len: u32,
        out_buf: BufId,
        out_off: u32,
    },
    /// Rename a path (overwriting an existing non-directory target).
    FileRename {
        old_buf: BufId,
        old_off: u32,
        old_len: u32,
        new_buf: BufId,
        new_off: u32,
        new_len: u32,
    },
    /// Unlink a non-directory path.
    FileUnlink {
        path_buf: BufId,
        path_off: u32,
        path_len: u32,
    },
    /// Create a directory at the given path.
    FileMkdir {
        path_buf: BufId,
        path_off: u32,
        path_len: u32,
    },
}

/// A `clock_gettime` reading in nanoseconds. A reading before the clock's
/// epoch, which only `CLOCK_REALTIME` can give, counts as zero.
#[cfg(unix)]
pub(crate) fn clock_ns(sec: i64, nsec: i64) -> u64 {
    match (u64::try_from(sec), u64::try_from(nsec)) {
        (Ok(sec), Ok(nsec)) => sec * 1_000_000_000 + nsec,
        (Err(_), _) | (_, Err(_)) => 0,
    }
}

/// The whole seconds of a `clock_gettime` reading, zero before the epoch.
#[cfg(unix)]
pub(crate) fn clock_secs(sec: i64) -> u64 {
    u64::try_from(sec).unwrap_or(0)
}

pub const ECONNRESET: i32 = 104;
pub const ECONNREFUSED: i32 = 111;
pub const EPIPE: i32 = 32;
pub const ETIME: i32 = 62;
pub const ECANCELED: i32 = 125;
pub const EADDRINUSE: i32 = 98;
pub const ENOENT: i32 = 2;
pub const EBADF: i32 = 9;
pub const EAGAIN: i32 = 11;
pub const EBUSY: i32 = 16;
pub const EINVAL: i32 = 22;
pub const ENFILE: i32 = 23;
pub const EMFILE: i32 = 24;
pub const ENAMETOOLONG: i32 = 36;
pub const ENOBUFS: i32 = 105;
pub const ENOTCONN: i32 = 107;
pub const EOVERFLOW: i32 = 75;

/// A byte count or slot index as a completion result. Linux never moves more
/// than `MAX_RW_COUNT` (below `i32::MAX`) bytes in one operation; a count
/// that does not fit the `i32` result reads as `-EOVERFLOW` instead of
/// wrapping into something that looks like a negated errno.
pub fn count_result<N: TryInto<i32>>(n: N) -> i32 {
    n.try_into().unwrap_or(-EOVERFLOW)
}

/// Signals a backend from another thread. See [`Io::waker`].
pub trait Wake: Send + Sync {
    /// Complete the backend's pending or next [`Op::AwaitWake`]. Never blocks.
    fn wake(&self);
}

/// All operation slots are in use.
#[derive(Debug, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "a unit struct has no fields to make private"
)]
pub struct Busy;

pub trait Io {
    /// Monotonic nanoseconds. Virtual in the simulator.
    fn now_ns(&self) -> u64;
    /// Wall-clock Unix time in seconds, for request signing. In the simulator
    /// this is a fixed epoch plus virtual time.
    fn unix_secs(&self) -> u64;
    /// Access a pool buffer. The caller must not touch a buffer that an
    /// in-flight operation references.
    fn buf(&mut self, id: BufId) -> &mut [u8];
    /// Access two different pool buffers at once, for operations that read
    /// one and write the other. Panics if `a == b`.
    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]);
    /// Name a file that will later be read with [`Op::ReadFile`].
    /// Initialization-time only: it may allocate. The file need not exist yet.
    fn register_file(&mut self, path: &str) -> io::Result<FileId>;
    /// Bind and listen in slot `sock`. Initialization-time only; may block.
    /// Returns the bound address (useful with port 0).
    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr>;
    /// As [`Io::listen`], with options. A backend that cannot honour an
    /// option refuses with `Unsupported`; it never ignores one.
    fn listen_with(
        &mut self,
        sock: Sock,
        addr: SocketAddr,
        opts: ListenOpts,
    ) -> io::Result<SocketAddr> {
        if opts == ListenOpts::DEFAULT {
            self.listen(sock, addr)
        } else {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }
    /// Bind and listen on a Unix-domain stream socket at `path` in slot
    /// `sock`. Initialization-time only; may block.
    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()>;
    /// Enable or disable `TCP_NODELAY` on TCP sockets listened or connected
    /// from now on. Ignored for UDP and Unix-domain sockets.
    fn no_delay(&mut self, _on: bool) {}
    /// Enable or disable `SO_KEEPALIVE` on TCP sockets listened or connected
    /// from now on. Ignored for UDP and Unix-domain sockets.
    fn keep_alive(&mut self, _on: bool) {}
    /// The probe timing that goes with [`Io::keep_alive`] on TCP sockets
    /// listened or connected from now on; `None` leaves the system's
    /// defaults. Accepted sockets take their listener's.
    fn keep_alive_timing(&mut self, _timing: Option<KeepAlive>) {}
    /// The address of the peer of the connection in slot `sock`, when the
    /// backend knows it: on the ring, for a socket accepted with
    /// [`Op::Accept`] (an armed accept installs sockets the kernel reports
    /// no address for); on the portable backend, for any connected TCP
    /// socket. `None` otherwise, and for Unix-domain sockets.
    fn peer_addr(&self, _sock: Sock) -> Option<SocketAddr> {
        None
    }
    /// Return a buffer-ring buffer named in an [`Op::ArmedRecv`] completion
    /// back to the backend's ring without submitting an operation.
    fn give_back(&mut self, id: BufId);
    /// Ask for the segment sizes of each TCP connection opened from now on:
    /// a connect that succeeds then completes with an encoded [`Segment`]
    /// in place of 0. May be changed at any time. A backend that cannot measure
    /// ignores this, and its connects complete with 0.
    fn measure_segment_sizes(&mut self, _on: bool) {}
    /// A handle another thread can use to complete an [`Op::AwaitWake`],
    /// and so bring a thread blocked in `tick` back to its caller.
    /// Initialization-time only: it may allocate. `None` from a backend
    /// that has no such thing.
    fn waker(&mut self) -> Option<std::sync::Arc<dyn Wake>> {
        None
    }
    /// Queue an operation. Performs no system call.
    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy>;
    /// Queue a linked chain of file operations atomically. Refuses with
    /// [`Busy`] if fewer than `ops.len()` operation slots are free.
    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), Busy>;
    /// Hand queued operations to the kernel and collect completions, in one
    /// system call. With `wait`, blocks until at least one completion.
    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()>;
}

#[cfg(test)]
mod tests;
