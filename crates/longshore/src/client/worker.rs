// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One I/O thread's side of a client: takes requests from its lane, runs
//! them on a store, and reports how each ended. It does no waiting of its
//! own, so the same code runs on a real thread, in the simulator, and under
//! a caller's event loop.

use super::lane::{Cell, Finished, Kind, Lane, Pool, Request};
use crate::io::{BufId, Completion, Io, Op, Token};
use crate::store::s3::S3;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::Arc;

/// How a worker lays its lane out over the store it is given.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// Operations in progress at most. The store must allow as many.
    pub operations: u32,
    /// The first buffer lent to callers, and how many follow it. They must
    /// be large buffers of the `Io`, used by nothing else.
    pub first_buffer: BufId,
    pub buffers: u32,
    /// Tokens used are `token_base` and `token_base + 1`.
    pub token_base: u64,
    /// How often the store's own timers run.
    pub heartbeat_ns: u64,
}

pub struct Worker {
    s3: S3,
    lane: Arc<Lane>,
    requests: Receiver<Request>,
    layout: Layout,
    stopping: bool,
}

impl Worker {
    /// Build a worker and its lane over `s3`, which must not have been
    /// started. Initialization: this allocates.
    pub fn new<IO: Io>(io: &mut IO, s3: S3, layout: Layout) -> (Worker, Arc<Lane>) {
        assert!(layout.operations >= 1 && layout.buffers >= 1);
        let wake = io
            .waker()
            .expect("the I/O backend has no way to be woken from another thread");
        let first = io.buf(layout.first_buffer);
        let (base, buf_size) = (first.as_mut_ptr(), first.len());
        if layout.buffers > 1 {
            let next = io.buf(BufId(layout.first_buffer.0 + 1)).as_mut_ptr();
            assert_eq!(
                next as usize - base as usize,
                buf_size,
                "lent buffers must be contiguous and of one size"
            );
        }
        let (tx, rx) = sync_channel(layout.operations as usize + 1);
        let lane = Arc::new(Lane {
            requests: tx,
            wake,
            cells: (0..layout.operations).map(|_| Cell::default()).collect(),
            ops: Pool::new(layout.operations),
            bufs: Pool::new(layout.buffers),
            base,
            buf_size,
            in_flight: AtomicU32::new(0),
            closed: AtomicBool::new(false),
        });
        let worker = Worker {
            s3,
            lane: lane.clone(),
            requests: rx,
            layout,
            stopping: false,
        };
        (worker, lane)
    }

    /// Start the store and the worker's own timer and wake-up. Call once.
    pub fn start<IO: Io>(&mut self, io: &mut IO) {
        self.s3.start(io);
        self.arm(io, 0);
        self.arm(io, 1);
    }

    fn arm<IO: Io>(&self, io: &mut IO, which: u64) {
        let op = if which == 0 {
            Op::AwaitWake
        } else {
            Op::Timeout {
                nanos: self.layout.heartbeat_ns,
            }
        };
        io.submit(Token(self.layout.token_base + which), op)
            .expect("Limits::ops_max must leave two operations for the worker");
    }

    /// Feed a completion. Returns false if it belongs to neither the worker
    /// nor its store.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> bool {
        match c.token.0.checked_sub(self.layout.token_base) {
            Some(0) => self.arm(io, 0),
            Some(1) => {
                self.s3.tick(io);
                self.arm(io, 1);
            }
            _ => return self.s3.on_completion(io, c),
        }
        true
    }

    /// Take what callers have asked for and report what has ended. Never
    /// waits. Call after feeding completions, and before waiting for more.
    pub fn turn<IO: Io>(&mut self, io: &mut IO) {
        loop {
            match self.requests.try_recv() {
                Ok(r) => self.begin(io, r),
                Err(TryRecvError::Empty) => break,
                // Every handle is gone: nothing more can be asked for.
                Err(TryRecvError::Disconnected) => {
                    self.stopping = true;
                    break;
                }
            }
        }
        while let Some(d) = self.s3.poll() {
            self.lane
                .finish(d.token as u32, Finished::Outcome(d.outcome));
        }
    }

    fn begin<IO: Io>(&mut self, io: &mut IO, r: Request) {
        let key = core::str::from_utf8(&r.key[..r.key_len as usize]).unwrap_or("");
        let buf = BufId(self.layout.first_buffer.0 + r.buf);
        let token = r.op as u64;
        let sent = match r.kind {
            Kind::Shutdown => {
                self.stopping = true;
                return;
            }
            _ if self.stopping => return self.lane.finish(r.op, Finished::Closed),
            Kind::Store {
                no_clobber: false,
                checksum,
            } => self.s3.store(io, token, key, buf, r.len, checksum),
            Kind::Store {
                no_clobber: true,
                checksum,
            } => self
                .s3
                .store_no_clobber(io, token, key, buf, r.len, checksum),
            Kind::Fetch { range } => self.s3.fetch(io, token, key, range, buf),
            Kind::Erase => self.s3.erase(io, token, key),
        };
        if let Err(e) = sent {
            self.lane.finish(r.op, Finished::Rejected(e));
        }
    }

    /// True once a shutdown was asked for and nothing is in progress. The
    /// owner then stops driving the worker and calls [`Worker::close`].
    pub fn finished(&self) -> bool {
        self.stopping && self.s3.in_progress() == 0
    }

    /// Mark the lane closed and end every request still queued.
    pub fn close(&mut self) {
        self.lane.closed.store(true, Ordering::Release);
        while let Ok(r) = self.requests.try_recv() {
            if !matches!(r.kind, Kind::Shutdown) {
                self.lane.finish(r.op, Finished::Closed);
            }
        }
    }

    /// The store this worker runs, for its counters.
    pub fn store(&self) -> &S3 {
        &self.s3
    }
}
