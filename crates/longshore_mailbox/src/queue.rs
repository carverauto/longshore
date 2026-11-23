// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Bounded lock-free MPSC queue for `Copy` messages with in-band stop and
//! atomic close.

use crate::sync::{
    unpoisoned, AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Mutex, Ordering, Thread, UnsafeCell,
};
use core::fmt;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use longshore_common::io::{Io, Token, Wake};
use std::sync::Arc;

mod receiver;

const CLOSED: u64 = 1 << 63;
const STOPPED: u64 = 1 << 62;
const COUNT_MASK: u64 = (1 << 62) - 1;

const NOTIFY_IDLE: usize = 0;
const NOTIFY_SIGNALED: usize = 1;
const NOTIFY_PARKED: usize = 2;

const PARK_NONE: u8 = 0;
const PARK_STOP: u8 = 1;
const PARK_RECV: u8 = 2;

/// Building a mailbox failed because the `Io` backend provided no waker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoWaker;

impl fmt::Display for NoWaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("I/O backend has no cross-thread waker")
    }
}

impl std::error::Error for NoWaker {}

/// Why [`QueueSender::try_send`] did not accept a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrySendError<M> {
    /// The queue is at its built capacity; the message is returned unchanged.
    Full(M),
    /// The queue was stopped or closed; the message is returned unchanged.
    Closed(M),
}

impl<M> TrySendError<M> {
    /// Return the unsent message.
    pub fn into_inner(self) -> M {
        match self {
            TrySendError::Full(m) | TrySendError::Closed(m) => m,
        }
    }
}

impl<M> fmt::Display for TrySendError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::Full(_) => f.write_str("mailbox queue is full"),
            TrySendError::Closed(_) => f.write_str("mailbox queue is closed"),
        }
    }
}

impl<M: fmt::Debug> std::error::Error for TrySendError<M> {}

/// An item delivered by the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv<M> {
    Message(M),
    Stop,
    Closed,
}

/// Non-blocking receive outcome from [`QueueReceiver::try_recv`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecv<M> {
    Message(M),
    Stop,
    Empty,
    Closed,
}

#[derive(Clone, Copy)]
enum Entry<M> {
    Msg(M),
    Stop,
}

struct Slot<M> {
    seq: AtomicUsize,
    val: UnsafeCell<MaybeUninit<Entry<M>>>,
}

// Safety: `seq` and `Shared::state`/`tail` synchronize slot ownership between
// the winning sender and the single receiver. `M: Copy` has no Drop glue.
unsafe impl<M: Send> Send for Slot<M> {}
unsafe impl<M: Send> Sync for Slot<M> {}

struct Shared<M> {
    slots: Box<[Slot<M>]>,
    cap: usize,
    state: AtomicU64,
    closed: AtomicBool,
    tail: AtomicUsize,
    senders: AtomicUsize,
    stop_requested: AtomicBool,
    park_mode: AtomicU8,
    wake: Arc<dyn Wake>,
    notify: AtomicUsize,
    parker: Mutex<Option<Thread>>,
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(&self) {}
}

/// Send end of a bounded lock-free MPSC queue.
pub struct QueueSender<M: Copy> {
    shared: Arc<Shared<M>>,
}

impl<M: Copy> Clone for QueueSender<M> {
    fn clone(&self) -> Self {
        let _: usize = self.shared.senders.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<M: Copy> Drop for QueueSender<M> {
    fn drop(&mut self) {
        if self.shared.senders.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.signal_park();
        }
    }
}

/// Receive end of a bounded lock-free MPSC queue, owned by a single core.
pub struct QueueReceiver<M: Copy> {
    shared: Arc<Shared<M>>,
    head: usize,
    stop_pending: bool,
    stop_seen: bool,
    closed: bool,
    wake_token: Token,
    armed: bool,
    _not_sync: PhantomData<core::cell::Cell<()>>,
}

/// Build a bounded lock-free MPSC queue backed by `io.waker()`.
pub fn queue<M: Copy, IO: Io>(
    io: &mut IO,
    capacity: usize,
    wake_token: Token,
) -> Result<(QueueSender<M>, QueueReceiver<M>), NoWaker> {
    let wake = io.waker().ok_or(NoWaker)?;
    Ok(queue_with_wake(wake, capacity, wake_token))
}

/// Build a bounded lock-free MPSC queue using an explicit [`Wake`] handle.
pub fn queue_with_wake<M: Copy>(
    wake: Arc<dyn Wake>,
    capacity: usize,
    wake_token: Token,
) -> (QueueSender<M>, QueueReceiver<M>) {
    build_queue(wake, capacity, wake_token, PARK_NONE)
}

fn build_queue<M: Copy>(
    wake: Arc<dyn Wake>,
    capacity: usize,
    wake_token: Token,
    park_mode: u8,
) -> (QueueSender<M>, QueueReceiver<M>) {
    assert!(capacity >= 1, "queue capacity must be at least 1");
    let total = capacity + 1;
    let slots: Box<[Slot<M>]> = (0..total)
        .map(|i| Slot {
            seq: AtomicUsize::new(i),
            val: UnsafeCell::new(MaybeUninit::uninit()),
        })
        .collect();
    let shared = Arc::new(Shared {
        slots,
        cap: capacity,
        state: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        tail: AtomicUsize::new(0),
        senders: AtomicUsize::new(1),
        stop_requested: AtomicBool::new(false),
        park_mode: AtomicU8::new(park_mode),
        wake,
        notify: AtomicUsize::new(NOTIFY_IDLE),
        parker: Mutex::new(None),
    });
    #[cfg(not(longshore_model))]
    drop(shared.parker.lock());
    (
        QueueSender {
            shared: shared.clone(),
        },
        QueueReceiver {
            shared,
            head: 0,
            stop_pending: false,
            stop_seen: false,
            closed: false,
            wake_token,
            armed: false,
            _not_sync: PhantomData,
        },
    )
}

/// Build a bounded lock-free MPSC queue without an `Io` backend, for
/// [`QueueReceiver::recv_parked`] / [`QueueReceiver::wait_stop`].
pub fn queue_parked<M: Copy>(capacity: usize) -> (QueueSender<M>, QueueReceiver<M>) {
    build_queue(Arc::new(NoopWake), capacity, Token::new(0), PARK_RECV)
}

impl<M: Copy> QueueSender<M> {
    fn unpark_if_parked(&self) {
        if self.shared.notify.swap(NOTIFY_SIGNALED, Ordering::AcqRel) == NOTIFY_PARKED {
            let guard = unpoisoned(self.shared.parker.lock());
            if let Some(t) = &*guard {
                t.unpark();
            }
        }
    }

    fn signal_msg(&self) {
        self.shared.wake.wake();
        if self.shared.park_mode.load(Ordering::Acquire) == PARK_RECV {
            self.unpark_if_parked();
        }
    }

    fn signal_park(&self) {
        self.shared.wake.wake();
        if self.shared.park_mode.load(Ordering::SeqCst) != PARK_NONE {
            self.unpark_if_parked();
        }
    }

    /// Wake the receiver without sending a message.
    pub fn wake(&self) {
        self.signal_park();
    }

    /// Monotonic advisory signal that [`QueueSender::stop`] or
    /// [`QueueReceiver::close`] has begun.
    ///
    /// Because the closed flag is published before `state` transitions, a
    /// concurrent [`QueueSender::try_send`] racing `stop` or `close` may still
    /// return `Ok(())` after `is_closed()` becomes `true`; any accepted message
    /// is delivered before `Stop` or drained by `close`.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// Enqueue `msg`, returning `Err(TrySendError::Full(msg))` when at
    /// capacity or `Err(TrySendError::Closed(msg))` after stop or close.
    pub fn try_send(&self, msg: M) -> Result<(), TrySendError<M>> {
        loop {
            let s = self.shared.state.load(Ordering::Acquire);
            if s & (CLOSED | STOPPED) != 0 {
                return Err(TrySendError::Closed(msg));
            }
            if (s & COUNT_MASK) >= self.shared.cap as u64 {
                return Err(TrySendError::Full(msg));
            }
            if self
                .shared
                .state
                .compare_exchange_weak(s, s + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        let pos = self.shared.tail.fetch_add(1, Ordering::AcqRel);
        let idx = pos % self.shared.slots.len();
        let slot = &self.shared.slots[idx];
        debug_assert_eq!(slot.seq.load(Ordering::Acquire), pos);
        slot.val.with_mut(|p| unsafe {
            let _: &mut Entry<M> = (*p).write(Entry::Msg(msg));
        });
        slot.seq.store(pos + 1, Ordering::Release);
        self.signal_msg();
        Ok(())
    }

    /// Request the receiver to stop.
    ///
    /// Uses the `+ 1` reserved slot so this succeeds even when `capacity`
    /// messages are queued. Messages sent before stop are delivered first;
    /// sends after stop are refused as [`TrySendError::Closed`]. Always wakes
    /// a parked receiver even if [`QueueReceiver::close`] already ran.
    pub fn stop(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.stop_requested.store(true, Ordering::SeqCst);
        let won = loop {
            let s = self.shared.state.load(Ordering::Acquire);
            if s & (CLOSED | STOPPED) != 0 {
                break false;
            }
            if self
                .shared
                .state
                .compare_exchange_weak(s, s | STOPPED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break true;
            }
        };
        if won {
            let pos = self.shared.tail.fetch_add(1, Ordering::AcqRel);
            let idx = pos % self.shared.slots.len();
            let slot = &self.shared.slots[idx];
            debug_assert_eq!(slot.seq.load(Ordering::Acquire), pos);
            slot.val.with_mut(|p| unsafe {
                let _: &mut Entry<M> = (*p).write(Entry::Stop);
            });
            slot.seq.store(pos + 1, Ordering::Release);
        }
        self.signal_park();
    }
}
