// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One I/O thread's side of a client: takes requests from its lane, runs
//! them on a store, and reports how each ended. It does no waiting of its
//! own, so the same code runs on a real thread, in the simulator, and under
//! a caller's event loop.

use super::lane::{Finished, Kind, Lane, Request};
use super::sync::{AtomicBool, AtomicU32, AtomicU64};
use crate::s3::S3;
use longshore_common::io::{BufId, Completion, Io, Op, Token};
use longshore_mailbox::{Config, NoWaker, Pool, Receiver, Recv, TryRecv};
use std::sync::atomic::Ordering;
use std::sync::Arc;

const DEFAULT_WAITERS: usize = 256;

/// How a worker lays its lane out over the store it is given.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub(crate) operations: u32,
    pub(crate) first_buffer: BufId,
    pub(crate) buffers: u32,
    pub(crate) token_base: u64,
    pub(crate) heartbeat_ns: u64,
}

pub struct Worker {
    s3: S3,
    lane: Arc<Lane>,
    rx: Receiver<Request, Finished>,
    layout: Layout,
    /// Operation slots the store is running.
    running: Box<[bool]>,
    stopping: bool,
}

impl Worker {
    /// Build a worker and its lane over `s3`, which must not have been
    /// started. Initialization: this allocates. Refused if `io` cannot be
    /// woken from another thread.
    pub fn new<IO: Io>(
        io: &mut IO,
        s3: S3,
        layout: Layout,
    ) -> Result<(Worker, Arc<Lane>), NoWaker> {
        Self::with_waiters(
            io,
            s3,
            layout,
            DEFAULT_WAITERS.max(layout.operations as usize),
        )
    }

    pub(super) fn with_waiters<IO: Io>(
        io: &mut IO,
        s3: S3,
        layout: Layout,
        waiters_prealloc: usize,
    ) -> Result<(Worker, Arc<Lane>), NoWaker> {
        assert!(layout.operations >= 1 && layout.buffers >= 1 && waiters_prealloc >= 1);
        let first = io.buf(layout.first_buffer);
        let (base, buf_size) = (first.as_mut_ptr(), first.len());
        if layout.buffers > 1 {
            let next = io
                .buf(BufId::new(layout.first_buffer.get() + 1))
                .as_mut_ptr();
            assert_eq!(
                next as usize - base as usize,
                buf_size,
                "lent buffers must be contiguous and of one size"
            );
        }
        let config = Config::new(
            layout.operations as usize,
            layout.operations,
            waiters_prealloc,
            Token::new(layout.token_base),
        );
        let (sender, rx) = longshore_mailbox::mailbox(io, config)?;
        let lane = Arc::new(Lane {
            sender,
            bufs: Pool::new(layout.buffers, waiters_prealloc),
            base,
            buf_size,
            in_flight: AtomicU32::new(0),
            reloads: [AtomicU64::new(0), AtomicU64::new(0)],
            void: AtomicBool::new(false),
            #[cfg(all(feature = "fault-injection", target_os = "linux"))]
            fault: Default::default(),
        });
        let worker = Worker {
            s3,
            lane: lane.clone(),
            rx,
            layout,
            running: vec![false; layout.operations as usize].into_boxed_slice(),
            stopping: false,
        };
        Ok((worker, lane))
    }

    /// Start the store and the worker's own timer and wake-up. Call once.
    pub fn start<IO: Io>(&mut self, io: &mut IO) {
        self.s3.start(io);
        #[expect(
            clippy::expect_used,
            reason = "the worker keeps two operations in flight, which Limits::ops_max must leave it"
        )]
        self.rx
            .arm(io)
            .expect("Limits::ops_max must leave two operations for the worker");
        self.arm_timer(io);
    }

    #[expect(
        clippy::expect_used,
        reason = "the worker keeps two operations in flight, which Limits::ops_max must leave it"
    )]
    fn arm_timer<IO: Io>(&self, io: &mut IO) {
        io.submit(
            Token::new(self.layout.token_base + 1),
            Op::Timeout {
                nanos: self.layout.heartbeat_ns,
            },
        )
        .expect("Limits::ops_max must leave two operations for the worker");
    }

    /// Feed a completion. Returns false if it belongs to neither the worker
    /// nor its store.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> bool {
        #[expect(
            clippy::expect_used,
            reason = "the worker keeps two operations in flight, which Limits::ops_max must leave it"
        )]
        let woken = self
            .rx
            .on_completion(io, c)
            .expect("Limits::ops_max must leave two operations for the worker");
        if woken {
            return true;
        }
        if c.token() == Token::new(self.layout.token_base + 1) {
            self.s3.tick(io);
            self.arm_timer(io);
            return true;
        }
        self.s3.on_completion(io, c)
    }

    /// Take what callers have asked for and report what has ended. Never
    /// waits. Call after feeding completions, and before waiting for more.
    pub fn turn<IO: Io>(&mut self, io: &mut IO) {
        loop {
            match self.rx.try_recv() {
                TryRecv::Message(r) => self.begin(io, &r),
                TryRecv::Stop => self.stopping = true,
                TryRecv::Empty => break,
                // Every handle is gone: nothing more can be asked for.
                TryRecv::Closed => {
                    self.stopping = true;
                    break;
                }
            }
        }
        self.report();
    }

    fn answer(&mut self, op: u32, finished: &Finished) {
        let _: u32 = self.lane.in_flight.fetch_sub(1, Ordering::Relaxed);
        if let Some(buf) = self.rx.answer(op, *finished) {
            self.lane.give_buffer(buf);
        }
    }

    fn report(&mut self) {
        for (shown, count) in self.lane.reloads.iter().zip([
            self.s3.credential_rotations(),
            self.s3.credential_failures(),
        ]) {
            shown.store(count, Ordering::Relaxed);
        }
        while let Some(d) = self.s3.poll() {
            self.running[d.token as usize] = false;
            self.answer(d.token as u32, &Finished::Outcome(d.outcome));
        }
    }

    fn begin<IO: Io>(&mut self, io: &mut IO, r: &Request) {
        if self.stopping {
            self.answer(r.op, &Finished::Closed);
            return;
        }
        let key = core::str::from_utf8(&r.key[..r.key_len as usize]).unwrap_or("");
        let buf = BufId::new(self.layout.first_buffer.get() + r.buf);
        let token = u64::from(r.op);
        let sent = match r.kind {
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
            Kind::Begin { checksums } => self.s3.multipart_begin(io, token, key, checksums),
            Kind::Part { checksum } => match &r.upload {
                Some(upload) => self
                    .s3
                    .multipart_store_part(io, token, key, upload, r.number, buf, r.len, checksum),
                None => Err(crate::s3::SubmitError::BadPart),
            },
            Kind::Complete => match &r.upload {
                Some(upload) => self
                    .s3
                    .multipart_complete(io, token, key, upload, buf, r.len, None),
                None => Err(crate::s3::SubmitError::BadPart),
            },
            Kind::Put { options } => {
                let limit = r.budget.limit(io);
                self.s3.put(io, token, key, buf, r.len, &options, limit)
            }
            Kind::Head => {
                let limit = r.budget.limit(io);
                self.s3.head(io, token, key, &r.expect, limit)
            }
            Kind::Get { range } => {
                let limit = r.budget.limit(io);
                self.s3.get(io, token, key, range, &r.expect, buf, limit)
            }
            Kind::Abort => match &r.upload {
                Some(upload) => self.s3.multipart_abort(io, token, key, upload),
                None => Err(crate::s3::SubmitError::BadPart),
            },
        };
        match sent {
            Ok(()) => self.running[r.op as usize] = true,
            Err(e) => self.answer(r.op, &Finished::Rejected(e)),
        }
    }

    /// True once a shutdown was asked for and nothing is in progress. The
    /// owner then stops driving the worker and calls [`Worker::close`].
    pub fn finished(&self) -> bool {
        self.stopping && self.s3.in_progress() == 0
    }

    /// Mark the lane closed and end every request still queued.
    pub fn close(&mut self) {
        let lane = &self.lane;
        let stopping = &mut self.stopping;
        self.rx.begin_close(|item| match item {
            Recv::Message(r) => {
                let _: u32 = lane.in_flight.fetch_sub(1, Ordering::Relaxed);
                if let Some(buf) =
                    lane.sender
                        .cells()
                        .answer(lane.sender.pool(), r.op, Finished::Closed)
                {
                    lane.give_buffer(buf);
                }
            }
            Recv::Stop => *stopping = true,
            Recv::Closed => {}
        });
        self.lane.bufs.wake_all();
    }

    /// The I/O under the store has failed: mark the lane closed and end
    /// every operation running or queued as closed. `quiet` says whether the
    /// `Io` is known to have stopped using the lent buffers (see
    /// `longshore_common::io::uring::UringIo::quiesce`); their memory must stay
    /// allocated either way. If not, the lane is void: callers get their
    /// operations back, but its buffers are empty and never lent again. The
    /// store is not driven after this.
    pub fn abandon(&mut self, quiet: bool) {
        if !quiet {
            self.lane.void.store(true, Ordering::Release);
        }
        self.report();
        self.close();
        for op in 0..self.running.len() {
            if core::mem::take(&mut self.running[op]) {
                // An operation that reports what was sent says so here too:
                // its request may have reached the store.
                let finished = match self.s3.abandoned(op as u64) {
                    Some(report) => Finished::Outcome(crate::s3::Outcome::Report(report)),
                    None => Finished::Closed,
                };
                self.answer(op as u32, &finished);
            }
        }
        let lane = &self.lane;
        self.rx
            .finish_close(|| Finished::Closed, |buf| lane.give_buffer(buf));
        self.lane.bufs.wake_all();
    }

    /// After [`Worker::abandon`]: block the calling thread until a shutdown is
    /// asked for.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn refuse_until_shutdown(&mut self) {
        self.rx.wait_stop();
    }

    /// The store this worker runs, for its counters.
    pub fn store(&self) -> &S3 {
        &self.s3
    }
}

impl Layout {
    /// `operations` in progress at most, the `buffers` large buffers from
    /// `first_buffer` lent to callers, the tokens `token_base` and
    /// `token_base + 1`, and the store's timers every `heartbeat_ns`.
    #[inline]
    pub const fn new(
        operations: u32,
        first_buffer: BufId,
        buffers: u32,
        token_base: u64,
        heartbeat_ns: u64,
    ) -> Layout {
        Layout {
            operations,
            first_buffer,
            buffers,
            token_base,
            heartbeat_ns,
        }
    }

    /// Operations in progress at most. The store must allow as many.
    #[inline]
    pub const fn operations(self) -> u32 {
        self.operations
    }

    /// The first buffer lent to callers. They must be large buffers of the
    /// `Io`, used by nothing else.
    #[inline]
    pub const fn first_buffer(self) -> BufId {
        self.first_buffer
    }

    /// How many buffers follow [`Layout::first_buffer`].
    #[inline]
    pub const fn buffers(self) -> u32 {
        self.buffers
    }

    /// Tokens used are `token_base` and `token_base + 1`.
    #[inline]
    pub const fn token_base(self) -> u64 {
        self.token_base
    }

    /// How often the store's own timers run.
    #[inline]
    pub const fn heartbeat_ns(self) -> u64 {
        self.heartbeat_ns
    }
}
