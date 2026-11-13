// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a caller's thread and one I/O thread share: operation cells, the
//! pools of operation slots and lent buffers, and the request queue. All of
//! it is allocated when the lane is built.

use crate::io::Wake;
use crate::store::s3::{Checksum, Outcome, SubmitError};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};

/// The longest key a request message carries.
pub const KEY_MAX: usize = 1024;

/// How an operation ended.
// Held in a preallocated cell and passed by value: boxing the large variant
// would allocate on every operation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Finished {
    Outcome(Outcome),
    Rejected(SubmitError),
    /// The client shut down before the operation ran.
    Closed,
}

/// What a caller asks an I/O thread to do. Fixed size, so the queue that
/// carries it is allocated once.
pub(super) struct Request {
    pub op: u32,
    pub kind: Kind,
    pub key: [u8; KEY_MAX],
    pub key_len: u16,
    /// The lent buffer the operation uses, as an index into the lane's.
    pub buf: u32,
    pub len: u32,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Store {
        no_clobber: bool,
        checksum: Checksum,
    },
    Fetch {
        range: Option<(u64, u64)>,
    },
    Erase,
    /// Finish what is in progress, then stop.
    Shutdown,
}

/// Indices that callers take and give back, waiting when none is free.
pub(super) struct Pool {
    free: Mutex<Vec<u32>>,
    returned: Condvar,
    /// Async callers waiting for an index.
    waiters: Mutex<Vec<Waker>>,
}

impl Pool {
    pub fn new(count: u32) -> Pool {
        Pool {
            free: Mutex::new((0..count).rev().collect()),
            returned: Condvar::new(),
            waiters: Mutex::new(Vec::with_capacity(count as usize)),
        }
    }

    pub fn take_blocking(&self) -> u32 {
        let mut free = self.free.lock().expect("pool lock");
        loop {
            if let Some(i) = free.pop() {
                return i;
            }
            free = self.returned.wait(free).expect("pool lock");
        }
    }

    pub fn poll_take(&self, cx: &mut Context<'_>) -> Poll<u32> {
        if let Some(i) = self.free.lock().expect("pool lock").pop() {
            return Poll::Ready(i);
        }
        self.waiters
            .lock()
            .expect("pool lock")
            .push(cx.waker().clone());
        // An index may have come back between the two locks.
        match self.free.lock().expect("pool lock").pop() {
            Some(i) => Poll::Ready(i),
            None => Poll::Pending,
        }
    }

    pub fn try_take(&self) -> Option<u32> {
        self.free.lock().expect("pool lock").pop()
    }

    pub fn give(&self, i: u32) {
        self.free.lock().expect("pool lock").push(i);
        self.returned.notify_one();
        let waiter = self.waiters.lock().expect("pool lock").pop();
        if let Some(w) = waiter {
            w.wake();
        }
    }
}

#[derive(Default)]
pub(super) struct Cell {
    pub state: Mutex<CellState>,
    pub done: Condvar,
}

#[derive(Default)]
pub(super) struct CellState {
    pub finished: Option<Finished>,
    pub waker: Option<Waker>,
    /// The lent buffer the operation holds, if any.
    pub buf: Option<u32>,
    /// The caller dropped its future: the I/O thread frees the cell and
    /// the buffer when the operation ends.
    pub detached: bool,
}

/// One I/O thread as its callers see it.
pub struct Lane {
    pub(super) requests: SyncSender<Request>,
    pub(super) wake: Arc<dyn Wake>,
    pub(super) cells: Box<[Cell]>,
    pub(super) ops: Pool,
    pub(super) bufs: Pool,
    /// Start of the lane's first lendable buffer, in the I/O thread's pool.
    pub(super) base: *mut u8,
    pub(super) buf_size: usize,
    /// Operations sent and not yet ended, for choosing the least busy lane.
    pub(super) in_flight: AtomicU32,
    pub(super) closed: AtomicBool,
}

// The raw pointer is to memory the I/O thread owns for as long as the lane
// lives. A buffer is touched by the caller only while it holds the buffer's
// index, and by the I/O thread only while an operation holds it.
unsafe impl Send for Lane {}
unsafe impl Sync for Lane {}

impl Lane {
    /// Record how operation `op` ended and tell whoever waits for it. Called
    /// on the I/O thread.
    pub(super) fn finish(&self, op: u32, finished: Finished) {
        let cell = &self.cells[op as usize];
        let mut s = cell.state.lock().expect("cell lock");
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        if s.detached {
            let buf = s.buf.take();
            *s = CellState::default();
            drop(s);
            if let Some(buf) = buf {
                self.bufs.give(buf);
            }
            self.ops.give(op);
            return;
        }
        s.finished = Some(finished);
        let waker = s.waker.take();
        drop(s);
        cell.done.notify_one();
        if let Some(w) = waker {
            w.wake();
        }
    }

    pub(super) fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::Relaxed)
    }
}
