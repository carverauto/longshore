// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Single-consumer receiver side of the bounded lock-free MPSC queue.

use super::{
    Entry, QueueReceiver, Recv, TryRecv, CLOSED, COUNT_MASK, NOTIFY_IDLE, NOTIFY_PARKED, PARK_NONE,
    PARK_RECV, PARK_STOP, STOPPED,
};
use crate::sync::{self, unpoisoned, Ordering};
use longshore_common::io::{Busy, Completion, Io, Op};

impl<M: Copy> QueueReceiver<M> {
    /// Submit `Op::AwaitWake` if not already armed or closed.
    pub fn arm<IO: Io>(&mut self, io: &mut IO) -> Result<(), Busy> {
        if self.armed || self.closed {
            return Ok(());
        }
        io.submit(self.wake_token, Op::AwaitWake)?;
        self.armed = true;
        Ok(())
    }

    /// Handle a completion; if it matches the receiver's wake token, re-arm
    /// `Op::AwaitWake` (unless closed) and return `Ok(true)`.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> Result<bool, Busy> {
        if c.token != self.wake_token {
            return Ok(false);
        }
        self.armed = false;
        self.arm(io)?;
        Ok(true)
    }

    /// True if [`QueueReceiver::close`] has been called.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Pop the next message or in-band stop without blocking.
    pub fn try_recv(&mut self) -> TryRecv<M> {
        if self.closed || self.stop_seen {
            return TryRecv::Closed;
        }
        let n = self.shared.slots.len();
        loop {
            let idx = self.head % n;
            let slot = &self.shared.slots[idx];
            if slot.seq.load(Ordering::Acquire) == self.head + 1 {
                let entry = slot.val.with(|p| unsafe { (*p).assume_init() });
                slot.seq.store(self.head + n, Ordering::Release);
                self.head += 1;
                match entry {
                    Entry::Msg(m) => {
                        let _: u64 = self.shared.state.fetch_sub(1, Ordering::Release);
                        return TryRecv::Message(m);
                    }
                    Entry::Stop => {
                        self.stop_pending = true;
                        continue;
                    }
                }
            }
            if self.stop_pending || self.shared.senders.load(Ordering::Acquire) == 0 {
                let s = self.shared.state.load(Ordering::Acquire);
                if (s & COUNT_MASK) == 0 {
                    if self.stop_pending {
                        self.stop_pending = false;
                        self.stop_seen = true;
                        return TryRecv::Stop;
                    }
                    if (s & STOPPED) == 0 {
                        return TryRecv::Closed;
                    }
                }
            }
            return TryRecv::Empty;
        }
    }

    /// Drain all currently queued items to `on_item`, returning `true` when
    /// the queue is closed (all senders dropped or already closed).
    pub fn drain(&mut self, mut on_item: impl FnMut(Recv<M>)) -> bool {
        loop {
            match self.try_recv() {
                TryRecv::Message(m) => on_item(Recv::Message(m)),
                TryRecv::Stop => on_item(Recv::Stop),
                TryRecv::Empty => return false,
                TryRecv::Closed => return true,
            }
        }
    }

    /// Wait by parking the calling thread until an item, stop, or close.
    ///
    /// # Panics
    ///
    /// Panics unless the queue was built with [`super::queue_parked`] (or
    /// [`crate::mailbox_parked`]); `Io`-backed queues must use [`Io::tick`] and
    /// [`QueueReceiver::drain`], or [`QueueReceiver::wait_stop`] when parking
    /// after I/O shutdown.
    pub fn recv_parked(&mut self) -> Recv<M> {
        {
            let mut g = unpoisoned(self.shared.parker.lock());
            *g = Some(sync::current());
        }
        assert_eq!(
            self.shared.park_mode.load(Ordering::Relaxed),
            PARK_RECV,
            "recv_parked requires a queue built with queue_parked or mailbox_parked"
        );
        loop {
            let _: usize = self.shared.notify.swap(NOTIFY_IDLE, Ordering::AcqRel);
            match self.try_recv() {
                TryRecv::Message(m) => return Recv::Message(m),
                TryRecv::Stop => return Recv::Stop,
                TryRecv::Closed => return Recv::Closed,
                TryRecv::Empty => {}
            }
            if self
                .shared
                .notify
                .compare_exchange(
                    NOTIFY_IDLE,
                    NOTIFY_PARKED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }
            sync::park();
        }
    }

    /// Park without an `Io` until [`super::QueueSender::stop`] is called or all
    /// senders drop. Safe across races with [`QueueReceiver::close`].
    pub fn wait_stop(&mut self) {
        if self.stop_seen || self.stop_pending {
            return;
        }
        {
            let mut g = unpoisoned(self.shared.parker.lock());
            *g = Some(sync::current());
        }
        if self.shared.park_mode.load(Ordering::Acquire) == PARK_NONE {
            // Only the receiver moves the mode off PARK_NONE, so there is no
            // competing writer whose value a failed exchange would report.
            let _: Option<u8> = self
                .shared
                .park_mode
                .compare_exchange(PARK_NONE, PARK_STOP, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
        }
        loop {
            let _: usize = self.shared.notify.swap(NOTIFY_IDLE, Ordering::AcqRel);
            if self.shared.stop_requested.load(Ordering::SeqCst)
                || self.shared.senders.load(Ordering::SeqCst) == 0
            {
                return;
            }
            if self
                .shared
                .notify
                .compare_exchange(
                    NOTIFY_IDLE,
                    NOTIFY_PARKED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }
            sync::park();
        }
    }

    /// Atomically close the queue and drain every accepted message and any
    /// unconsumed stop to `on_item`.
    pub fn close(&mut self, mut on_item: impl FnMut(Recv<M>)) {
        if self.closed {
            return;
        }
        self.shared.closed.store(true, Ordering::Release);
        let prev = self.shared.state.fetch_or(CLOSED, Ordering::AcqRel);
        let rem_msgs = (prev & COUNT_MASK) as usize;
        let rem_stop = (prev & STOPPED != 0) && !self.stop_seen && !self.stop_pending;
        let total = rem_msgs + usize::from(rem_stop);
        let n = self.shared.slots.len();
        for _ in 0..total {
            let idx = self.head % n;
            let slot = &self.shared.slots[idx];
            while slot.seq.load(Ordering::Acquire) != self.head + 1 {
                sync::yield_now();
            }
            let entry = slot.val.with(|p| unsafe { (*p).assume_init() });
            slot.seq.store(self.head + n, Ordering::Release);
            self.head += 1;
            match entry {
                Entry::Msg(m) => {
                    let _: u64 = self.shared.state.fetch_sub(1, Ordering::Release);
                    on_item(Recv::Message(m));
                }
                Entry::Stop => {
                    self.stop_pending = true;
                }
            }
        }
        if self.stop_pending {
            self.stop_pending = false;
            self.stop_seen = true;
            on_item(Recv::Stop);
        }
        self.closed = true;
    }
}
