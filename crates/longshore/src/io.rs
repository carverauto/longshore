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
use std::net::SocketAddrV4;

pub mod alloc_guard;
#[cfg(target_os = "linux")]
pub mod ring;
#[cfg(target_os = "linux")]
pub mod sys;
#[cfg(target_os = "linux")]
pub mod uring;

/// Caller-chosen identifier returned in the matching [`Completion`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token(pub u64);

/// Index into the buffer pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufId(pub u32);

/// A file registered at initialization with [`Io::register_file`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileId(pub u32);

/// Index into the socket table. The caller assigns slots.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sock(pub u32);

#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Open a TCP socket in slot `sock` and connect it. A connect that fails
    /// leaves the slot free; the caller need not close it.
    Connect {
        sock: Sock,
        addr: SocketAddrV4,
    },
    /// Open a UDP socket in slot `sock` connected to `addr`. `Send` and `Recv`
    /// then carry whole datagrams.
    ConnectUdp {
        sock: Sock,
        addr: SocketAddrV4,
    },
    /// Accept one connection from `listener` into slot `sock`.
    Accept {
        listener: Sock,
        sock: Sock,
    },
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
    Close {
        sock: Sock,
    },
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
    Timeout {
        nanos: u64,
    },
    /// Ask for the in-flight operation submitted with `target` to stop. That
    /// operation then completes, normally with `-ECANCELED`, or with its real
    /// result if it finished first. The cancel itself completes with 0, or a
    /// negated errno if no such operation was found.
    Cancel {
        target: Token,
    },
}

/// `result` is a byte count or 0 on success, or a negated Linux errno.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
    pub token: Token,
    pub result: i32,
}

pub const ECONNRESET: i32 = 104;
pub const ECONNREFUSED: i32 = 111;
pub const EPIPE: i32 = 32;
pub const ETIME: i32 = 62;
pub const ECANCELED: i32 = 125;
pub const ENOENT: i32 = 2;
pub const EAGAIN: i32 = 11;

/// Fixed-capacity completion buffer; `tick` never delivers more than fit.
pub struct Completions {
    items: Box<[Completion]>,
    len: usize,
}

impl Completions {
    pub fn new(capacity: usize) -> Completions {
        Completions {
            items: vec![
                Completion {
                    token: Token(0),
                    result: 0
                };
                capacity
            ]
            .into_boxed_slice(),
            len: 0,
        }
    }
    pub fn is_full(&self) -> bool {
        self.len == self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn len(&self) -> usize {
        self.len
    }
    /// Caller must check `is_full` first.
    pub fn push(&mut self, c: Completion) {
        self.items[self.len] = c;
        self.len += 1;
    }
    pub fn pop(&mut self) -> Option<Completion> {
        if self.len == 0 {
            return None;
        }
        // Deliver in arrival order.
        let c = self.items[0];
        self.items.copy_within(1..self.len, 0);
        self.len -= 1;
        Some(c)
    }
}

/// Everything the I/O layer allocates is sized from these, once.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub sockets_max: u32,
    /// Large buffers, sized for object bodies: ids `0..buffers`.
    pub buffers: u32,
    pub buffer_size: u32,
    /// Small buffers, sized for request and response heads and datagrams:
    /// ids `buffers..buffers + small_buffers`. May be zero.
    pub small_buffers: u32,
    pub small_buffer_size: u32,
    /// Maximum operations in flight.
    pub ops_max: u32,
}

impl Limits {
    /// Buffers of both sizes.
    pub fn buffer_count(&self) -> u32 {
        self.buffers + self.small_buffers
    }

    /// The id of small buffer `i`.
    pub fn small(&self, i: u32) -> BufId {
        assert!(i < self.small_buffers, "small buffer out of range");
        BufId(self.buffers + i)
    }

    /// Bytes in the one pool that holds every buffer, large ones first.
    pub fn pool_bytes(&self) -> usize {
        self.buffers as usize * self.buffer_size as usize
            + self.small_buffers as usize * self.small_buffer_size as usize
    }

    /// Where buffer `id` lies in the pool: its offset and its length.
    pub fn span(&self, id: BufId) -> (usize, usize) {
        assert!(id.0 < self.buffer_count(), "buffer out of range");
        if id.0 < self.buffers {
            (
                id.0 as usize * self.buffer_size as usize,
                self.buffer_size as usize,
            )
        } else {
            let large = self.buffers as usize * self.buffer_size as usize;
            (
                large + (id.0 - self.buffers) as usize * self.small_buffer_size as usize,
                self.small_buffer_size as usize,
            )
        }
    }

    /// Like [`Limits::span`] for the range `off..off + len` of the buffer.
    pub fn range(&self, id: BufId, off: u32, len: u32) -> core::ops::Range<usize> {
        let (base, size) = self.span(id);
        assert!(
            off as u64 + len as u64 <= size as u64,
            "buffer range out of bounds"
        );
        base + off as usize..base + off as usize + len as usize
    }

    /// Two distinct buffers of the pool, borrowed at once.
    pub fn pair<'a>(&self, pool: &'a mut [u8], a: BufId, b: BufId) -> (&'a mut [u8], &'a mut [u8]) {
        assert!(a != b, "buf_pair needs two distinct pool buffers");
        let ((a_at, a_len), (b_at, b_len)) = (self.span(a), self.span(b));
        if a_at < b_at {
            let (left, right) = pool.split_at_mut(b_at);
            (&mut left[a_at..a_at + a_len], &mut right[..b_len])
        } else {
            let (left, right) = pool.split_at_mut(a_at);
            (&mut right[..a_len], &mut left[b_at..b_at + b_len])
        }
    }
}

/// All operation slots are in use.
#[derive(Debug, PartialEq, Eq)]
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
    fn listen(&mut self, sock: Sock, addr: SocketAddrV4) -> io::Result<SocketAddrV4>;
    /// Queue an operation. Performs no system call.
    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy>;
    /// Hand queued operations to the kernel and collect completions, in one
    /// system call. With `wait`, blocks until at least one completion.
    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()>;
}

#[cfg(test)]
mod tests;
