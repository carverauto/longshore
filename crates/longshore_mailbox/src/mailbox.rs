// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Combined mailbox pairing a bounded lock-free queue with a slot [`Pool`]
//! and answer [`Cells`].

use crate::cells::Cells;
use crate::future::CallFuture;
use crate::pool::Pool;
use crate::queue::{
    queue, queue_parked, queue_with_wake, NoWaker, QueueReceiver, QueueSender, Recv, TryRecv,
    TrySendError,
};
use core::task::{Context, Poll};
use longshore_common::io::{Busy, Completion, Io, Token, Wake};
use std::sync::Arc;

/// Configuration for building a slotted mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Maximum queued messages (excluding the reserved in-band stop slot).
    pub capacity: usize,
    /// Number of caller waiter slots (`0..slots`).
    pub slots: u32,
    /// Preallocated waiter queue capacity inside [`Pool`].
    pub waiters_prealloc: usize,
    /// Token used for `Op::AwaitWake` on the receiver's `Io`.
    pub wake_token: Token,
}

/// Caller-facing handle to a slotted mailbox.
pub struct Sender<M: Copy, A> {
    tx: QueueSender<M>,
    pool: Arc<Pool>,
    cells: Arc<Cells<A>>,
}

impl<M: Copy, A> Clone for Sender<M, A> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            pool: self.pool.clone(),
            cells: self.cells.clone(),
        }
    }
}

/// Core-facing receiver for a slotted mailbox.
pub struct Receiver<M: Copy, A> {
    rx: QueueReceiver<M>,
    pool: Arc<Pool>,
    cells: Arc<Cells<A>>,
}

/// A paired [`Sender`] and [`Receiver`] returned by [`mailbox`].
pub type Mailbox<M, A> = (Sender<M, A>, Receiver<M, A>);

/// Build a slotted mailbox integrated with `io.waker()`.
pub fn mailbox<M: Copy, A, IO: Io>(io: &mut IO, config: Config) -> Result<Mailbox<M, A>, NoWaker> {
    let (tx, rx) = queue(io, config.capacity, config.wake_token)?;
    Ok(assemble(tx, rx, config))
}

/// Build a slotted mailbox using an explicit [`Wake`] handle.
pub fn mailbox_with_wake<M: Copy, A>(wake: Arc<dyn Wake>, config: Config) -> Mailbox<M, A> {
    let (tx, rx) = queue_with_wake(wake, config.capacity, config.wake_token);
    assemble(tx, rx, config)
}

/// Build a slotted mailbox without an `Io` backend (for parked receive tests).
pub fn mailbox_parked<M: Copy, A>(config: Config) -> Mailbox<M, A> {
    let (tx, rx) = queue_parked(config.capacity);
    assemble(tx, rx, config)
}

fn assemble<M: Copy, A>(tx: QueueSender<M>, rx: QueueReceiver<M>, config: Config) -> Mailbox<M, A> {
    let pool = Arc::new(Pool::new(config.slots, config.waiters_prealloc));
    let cells = Arc::new(Cells::new(config.slots));
    (
        Sender {
            tx,
            pool: pool.clone(),
            cells: cells.clone(),
        },
        Receiver { rx, pool, cells },
    )
}

impl<M: Copy, A> Sender<M, A> {
    /// Slot pool shared with the receiver.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Answer cells shared with the receiver.
    pub fn cells(&self) -> &Cells<A> {
        &self.cells
    }

    /// Monotonic advisory signal that [`Sender::stop`] or
    /// [`Receiver::begin_close`] has begun.
    ///
    /// Because the closed flag is published before the queue state transitions,
    /// a concurrent [`Sender::try_send`] or [`Sender::send_slot`] racing `stop`
    /// or `begin_close` may still return `Ok(())` after `is_closed()` becomes
    /// `true`; any accepted message is delivered before `Stop` or drained by
    /// `begin_close`.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Wake the receiver without sending a message.
    pub fn wake(&self) {
        self.tx.wake();
    }

    /// Request an in-band stop, waking both the receiver and any parked slot
    /// waiters.
    pub fn stop(&self) {
        self.tx.stop();
        self.pool.wake_all();
    }

    /// Send a raw message without attaching a waiter slot.
    pub fn try_send(&self, msg: M) -> Result<(), TrySendError<M>> {
        self.tx.try_send(msg)
    }

    /// Mark `slot` as sent with optional caller `tag` and enqueue `msg`. On
    /// failure, rolls back cell state (the caller still owns `slot`).
    pub fn send_slot(&self, slot: u32, tag: Option<u32>, msg: M) -> Result<(), TrySendError<M>> {
        self.cells.prepare(slot, tag);
        if let Err(err) = self.tx.try_send(msg) {
            self.cells.cancel_prepare(slot);
            return Err(err);
        }
        Ok(())
    }

    /// Poll `slot` for an answer, freeing `slot` into the pool when ready.
    pub fn poll_answer(&self, slot: u32, cx: &mut Context<'_>) -> Poll<A> {
        self.cells.poll_answer(&self.pool, slot, cx)
    }

    /// Block until `slot` is answered, then free `slot` and return the answer.
    pub fn wait_answer(&self, slot: u32) -> A {
        self.cells.wait_answer(&self.pool, slot)
    }

    /// Give up waiting on `slot`. Returns `Some(answer)` if already answered
    /// (having freed `slot`), or `None` after marking `slot` detached.
    pub fn detach(&self, slot: u32) -> Option<A> {
        self.cells.detach(&self.pool, slot)
    }

    /// High-level call future for single-mailbox callers with no extra tag.
    pub fn call<F: FnOnce(u32) -> M>(&self, build: F, closed: A) -> CallFuture<'_, M, A, F> {
        CallFuture::new(self, build, closed)
    }
}

impl<M: Copy, A> Receiver<M, A> {
    /// Slot pool shared with callers.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Answer cells shared with callers.
    pub fn cells(&self) -> &Cells<A> {
        &self.cells
    }

    /// Submit `Op::AwaitWake` if not already armed or closed.
    pub fn arm<IO: Io>(&mut self, io: &mut IO) -> Result<(), Busy> {
        self.rx.arm(io)
    }

    /// Handle a completion; re-arms `Op::AwaitWake` when it matches `wake_token`.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> Result<bool, Busy> {
        self.rx.on_completion(io, c)
    }

    /// Pop one item without blocking.
    pub fn try_recv(&mut self) -> TryRecv<M> {
        self.rx.try_recv()
    }

    /// Drain currently queued items, returning `true` when closed.
    pub fn drain(&mut self, on_item: impl FnMut(Recv<M>)) -> bool {
        self.rx.drain(on_item)
    }

    /// Receive by parking without an `Io` backend.
    ///
    /// # Panics
    ///
    /// Panics unless the mailbox was built with [`mailbox_parked`]; `Io`-backed
    /// mailboxes must use [`Io::tick`] and [`Receiver::drain`], or
    /// [`Receiver::wait_stop`] when parking after I/O shutdown.
    pub fn recv_parked(&mut self) -> Recv<M> {
        self.rx.recv_parked()
    }

    /// Park without an `Io` backend until stopped or all senders drop.
    pub fn wait_stop(&mut self) {
        self.rx.wait_stop();
    }

    /// Answer `slot` with `value`. If detached, frees `slot` FIRST and returns
    /// any caller-attached `u32` tag.
    pub fn answer(&mut self, slot: u32, value: A) -> Option<u32> {
        self.cells.answer(&self.pool, slot, value)
    }

    /// Phase 1 of closing: atomically close the queue, drain remaining queued
    /// items to `on_item`, and wake all slot waiters.
    pub fn begin_close(&mut self, on_item: impl FnMut(Recv<M>)) {
        self.rx.close(on_item);
        self.pool.wake_all();
    }

    /// Phase 2 of closing: after answering any in-progress slots that can still
    /// report an outcome, finish every remaining sent-and-unanswered slot with
    /// `closed()`, freeing detached slots before invoking `on_detached(tag)`.
    pub fn finish_close(&mut self, closed: impl FnMut() -> A, on_detached: impl FnMut(u32)) {
        self.cells.close_unanswered(&self.pool, closed, on_detached);
        self.pool.wake_all();
    }

    /// Single-step close: drains remaining items, answers remaining
    /// sent-and-unanswered slots with `closed()`, and wakes all slot waiters.
    pub fn close(
        &mut self,
        on_item: impl FnMut(Recv<M>),
        closed: impl FnMut() -> A,
        on_detached: impl FnMut(u32),
    ) {
        self.begin_close(on_item);
        self.finish_close(closed, on_detached);
    }
}
