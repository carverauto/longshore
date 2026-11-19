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
    Completion, Completions, FileId, Limits, Op, Sock, Token, Wake, EBADF, ECANCELED, ENOENT,
};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod backend;
mod step;

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
}

#[derive(Clone, Copy)]
pub(crate) struct Pending {
    pub(crate) token: Token,
    pub(crate) op: Op,
    pub(crate) deadline: u64,
    pub(crate) cancelled: bool,
    pub(crate) id: u64,
    pub(crate) cancel_target: Option<u64>,
    pub(crate) attached: bool,
    pub(crate) fresh: bool,
    pub(crate) chain_id: u64,
    pub(crate) chain_pos: u32,
    pub(crate) chain_active: bool,
    pub(crate) chain_aborted: bool,
    pub(crate) world_tag: u64,
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
            trace: 0xcbf2_9ce4_8422_2325,
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
    pub(crate) fn reset_for_power_loss(&mut self) {
        self.pending.clear();
        self.ready.clear();
        self.files.reset();
        self.woken.store(false, Ordering::Release);
    }
}
