// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a caller's thread and one I/O thread share: operation cells, the
//! pools of operation slots and lent buffers, and the request queue. All of
//! it is allocated when the lane is built.

use crate::store::s3::{Checksum, Outcome, SubmitError};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;

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
///
/// A waiter is named by an id from [`waiter_id`] and holds at most one
/// place in the queue, which it leaves when it takes an index or gives up.
pub(super) struct Pool {
    state: Mutex<PoolState>,
}

struct PoolState {
    free: Vec<u32>,
    waiters: VecDeque<(u64, Waker)>,
}

impl PoolState {
    /// Wake the longest waiting caller if an index is free for it.
    fn pass_on(&mut self) {
        if !self.free.is_empty() {
            if let Some((_, w)) = self.waiters.pop_front() {
                w.wake();
            }
        }
    }
}

/// A fresh name for a caller that may wait on a [`Pool`].
pub(super) fn waiter_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Pool {
    pub fn new(count: u32) -> Pool {
        Pool {
            state: Mutex::new(PoolState {
                free: (0..count).rev().collect(),
                waiters: VecDeque::with_capacity(count as usize),
            }),
        }
    }

    /// Take an index, or queue waiter `id` to be woken when one comes back.
    /// Polling again replaces the waker the waiter left before.
    pub fn poll_take(&self, id: u64, cx: &mut Context<'_>) -> Poll<u32> {
        let mut s = self.state.lock().expect("pool lock");
        if let Some(i) = s.free.pop() {
            s.waiters.retain(|(w, _)| *w != id);
            return Poll::Ready(i);
        }
        match s.waiters.iter_mut().find(|(w, _)| *w == id) {
            Some((_, waker)) => waker.clone_from(cx.waker()),
            None => s.waiters.push_back((id, cx.waker().clone())),
        }
        Poll::Pending
    }

    /// Waiter `id` no longer waits here. If it was woken for an index it
    /// did not take, the wake passes to the next waiter.
    pub fn forget(&self, id: u64) {
        let mut s = self.state.lock().expect("pool lock");
        s.waiters.retain(|(w, _)| *w != id);
        s.pass_on();
    }

    pub fn give(&self, i: u32) {
        let mut s = self.state.lock().expect("pool lock");
        s.free.push(i);
        s.pass_on();
    }
}

/// Run `f` to its end on the calling thread, parking between polls.
pub(super) fn block_on<F: Future>(f: F) -> F::Output {
    struct Parker(Thread);
    impl Wake for Parker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    thread_local! {
        static PARKER: Waker = Waker::from(Arc::new(Parker(std::thread::current())));
    }
    let mut f = pin!(f);
    PARKER.with(|waker| {
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park();
        }
    })
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
    pub(super) wake: Arc<dyn crate::io::Wake>,
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
