// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared `Io` state machine generic over a [`World`].
//!
//! [`Engine`] owns the buffer pool, pending operation table, operation
//! identifiers, waker flag, and completion trace. A [`World`] supplies the
//! clock, seeded or rotating scheduling, operation attempts, and waiting.

use crate::io::files::FileTable;
use crate::io::{
    BufId, Completion, Completions, FileId, Limits, Op, Sock, Token, Wake, EBADF, ECANCELED,
    ENFILE, ENOBUFS, ENOENT,
};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod backend;
mod step;
mod submit;
mod taken;
#[cfg(test)]
mod tests;

/// Result of attempting a pending operation in a [`World`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attempt {
    pub(crate) result: i32,
    pub(crate) more: bool,
    pub(crate) buf: Option<BufId>,
}

impl Attempt {
    /// A single final completion carrying no selected buffer.
    pub const fn single(result: i32) -> Self {
        Self {
            result,
            more: false,
            buf: None,
        }
    }
}

/// What a concrete backend provides to [`Engine`].
pub trait World {
    /// Current monotonic time in nanoseconds.
    fn now_ns(&self) -> u64;
    /// Current wall-clock Unix time in seconds.
    fn unix_secs(&self) -> u64;
    /// Register a file path for later [`Op::ReadFile`].
    fn register_file(&mut self, path: &str) -> io::Result<FileId>;
    /// Bind and listen on `sock` at `addr`, returning the bound address.
    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr>;
    /// As [`World::listen`], with options; refuses those it cannot honour.
    fn listen_with(
        &mut self,
        sock: Sock,
        addr: SocketAddr,
        opts: crate::io::ListenOpts,
    ) -> io::Result<SocketAddr> {
        if opts == crate::io::ListenOpts::DEFAULT {
            self.listen(sock, addr)
        } else {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }
    /// Bind and listen on a Unix-domain stream socket at `path` in `sock`.
    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()>;
    /// Enable or disable `TCP_NODELAY` on TCP sockets listened or connected from now on.
    fn no_delay(&mut self, _on: bool) {}
    /// Enable or disable `SO_KEEPALIVE` on TCP sockets listened or connected from now on.
    fn keep_alive(&mut self, _on: bool) {}
    /// The probe timing that goes with `keep_alive`.
    fn keep_alive_timing(&mut self, _timing: Option<crate::io::KeepAlive>) {}
    /// The peer's address of the connection in `sock`, if known.
    fn peer_addr(&self, _sock: Sock) -> Option<SocketAddr> {
        None
    }
    /// Return a buffer-ring buffer back to the world's ring.
    fn give_back(&mut self, _id: BufId) {}
    /// Ask to measure TCP segment sizes on future connects, if supported.
    fn measure_segment_sizes(&mut self, _on: bool) {}
    /// Build a cross-thread wake handle tied to `woken`, if supported.
    fn waker(&mut self, _woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        None
    }

    /// Whether `op`'s descriptor may currently be ready for progress.
    fn is_ready(&self, op: &Op) -> bool;

    /// Advance world state once at the start of each step (even when idle).
    fn advance(&mut self);
    /// Starting index in `0..n` for this step's pass over pending ops.
    fn first(&mut self, n: usize) -> usize;
    /// Whether `op` should be held back for this step.
    fn hold(&mut self, op: &Op) -> bool;
    /// Attempt one non-timer/non-cancel/non-wake operation; `None` means not ready.
    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32>;
    /// Whether an incomplete `op` currently holds a resource a cancel must release.
    fn claimed(&self, op: &Op) -> bool;
    /// Release a resource claimed by a cancelled `op`.
    fn release(&mut self, op: &Op);
    /// Notify the world after an [`Op::Close`] on `sock` completes.
    fn closed(&mut self, sock: Sock);
    /// Wait when `tick(true, ...)` has no completions yet.
    fn wait(&mut self, deadline: Option<u64>, only_timers: bool, idle: u32) -> io::Result<()>;

    /// How many steps this world goes on reading the buffer of an
    /// [`Op::SendZc`] whose bytes it has just taken. Zero for a world that
    /// copied them: the send then reports once.
    fn linger(&mut self, _op: &Op) -> u64 {
        0
    }

    /// Whether a report that is due now comes in a later step instead: the
    /// end of an operation its socket's close cancelled, or a close before
    /// the operations it ends. A real kernel fixes no order among them.
    fn late(&mut self) -> bool {
        false
    }
    /// Optional notification when a pending operation is marked cancelled.
    fn on_cancel(&mut self, _op: &Op) {}
    /// Optional notification before `wait` to clear tracked wait interest.
    fn clear_interest(&mut self) {}
    /// Optional notification before `wait` for each non-cancelled pending op.
    fn add_interest(&mut self, _op: &Op) {}
    /// Optional notification when an operation is accepted at submission.
    /// Returns an opaque tag attached to the pending operation.
    fn on_submit(&mut self, _op: &Op) -> u64 {
        0
    }
    /// Attempt an operation with its submission tag. Defaults to [`World::attempt`].
    fn attempt_tagged(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        _tag: u64,
    ) -> Option<i32> {
        self.attempt(op, pool, limits)
    }
    /// Attempt an operation with its submission tag, returning completion
    /// metadata (`more`, `buf`). Defaults to [`World::attempt_tagged`].
    fn attempt_completion(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        tag: u64,
    ) -> Option<Attempt> {
        self.attempt_tagged(op, pool, limits, tag)
            .map(Attempt::single)
    }
    /// Attempt a submitted `Op::ConnectUnix` using the path captured at submit time.
    fn attempt_connect_unix(&mut self, _sock: Sock, _path_with_nul: &[u8]) -> Option<i32> {
        None
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Pending {
    pub(crate) token: Token,
    pub(crate) op: Op,
    pub(crate) deadline: u64,
    pub(crate) cancelled: bool,
    /// Cancelled because its socket was closed, not by an `Op::Cancel`.
    pub(crate) closed_under: bool,
    pub(crate) id: u64,
    pub(crate) cancel_target: Option<u64>,
    pub(crate) attached: bool,
    pub(crate) fresh: bool,
    pub(crate) chain_id: u64,
    pub(crate) chain_pos: u32,
    pub(crate) chain_active: bool,
    pub(crate) chain_aborted: bool,
    pub(crate) world_tag: u64,
    pub(crate) last_delivered_step: u64,
    pub(crate) unix_path: [u8; 108],
    pub(crate) unix_len: u8,
    /// For an [`Op::SendZc`] whose bytes were taken: the bytes, and the
    /// step from which the world no longer reads its buffer.
    pub(crate) taken: Option<(i32, u64)>,
    /// What those bytes hashed to when they were taken.
    pub(crate) taken_sum: u64,
}

/// [`crate::io::Io`] implementation backed by a [`World`].
pub struct Engine<W> {
    pub(crate) limits: Limits,
    pub(crate) world: W,
    pub(crate) bufs: Box<[u8]>,
    pub(crate) pool_base: usize,
    pub(crate) pending: Vec<Pending>,
    pub(crate) ready: Completions,
    pub(crate) files: FileTable,
    pub(crate) next_op_id: u64,
    pub(crate) next_chain_id: u64,
    pub(crate) step_seq: u64,
    pub(crate) trace: u64,
    pub(crate) woken: Arc<AtomicBool>,
}

impl<W: World> Engine<W> {
    /// Construct an engine from `limits` and an initialized `world`.
    pub fn with_world(limits: Limits, world: W) -> Self {
        limits.validate();
        let bufs = vec![0u8; limits.pool_bytes()].into_boxed_slice();
        let pool_base = limits.pool_base(bufs.as_ptr() as usize);
        Self {
            limits,
            world,
            bufs,
            pool_base,
            pending: Vec::with_capacity(limits.ops_max as usize),
            ready: Completions::new(limits.ops_max as usize),
            files: FileTable::new(limits.files_max),
            next_op_id: 0,
            next_chain_id: 0,
            step_seq: 0,
            trace: super::trace::START,
            woken: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Hash of every completion delivered so far (token, result, time).
    pub fn trace_hash(&self) -> u64 {
        self.trace
    }

    /// Immutable access to the underlying world.
    pub fn world(&self) -> &W {
        &self.world
    }

    /// Mutable access to the underlying world.
    pub fn world_mut(&mut self) -> &mut W {
        &mut self.world
    }

    /// Drop all in-flight state, ready completions, file slots, and wake flag
    /// without allocating. Used by simulated whole-machine power loss.
    #[cfg(any(test, feature = "sim"))]
    pub(crate) fn reset_for_power_loss(&mut self) {
        self.pending.clear();
        self.ready.clear();
        self.files.reset();
        self.woken.store(false, Ordering::Release);
    }
}

impl Attempt {
    /// A completion with `result`, saying whether `more` follow and which
    /// buffer an armed receive selected.
    #[inline]
    pub const fn new(result: i32, more: bool, buf: Option<BufId>) -> Self {
        Self { result, more, buf }
    }

    /// A byte count or 0 on success, or a negated Linux errno.
    #[inline]
    pub const fn result(self) -> i32 {
        self.result
    }

    /// Whether more completions will follow for the operation.
    #[inline]
    pub const fn more(self) -> bool {
        self.more
    }

    /// Buffer selected by an armed receive, if any.
    #[inline]
    pub const fn buf(self) -> Option<BufId> {
        self.buf
    }
}
