// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! An operation in progress: a future, with a blocking wait as well.

use super::lane::{block_on, waiter_id, Finished, Kind, Lane, Request};
use super::{least_busy, open, request, Alive, Buffer, Error};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TrySendError;
use std::sync::Arc;
use std::task::{Context, Poll};

/// What an operation gives its caller: a result, or a result and the buffer
/// the operation borrowed.
pub trait Output: Sized {
    type Value;
    #[doc(hidden)]
    fn build(result: Result<Self::Value, Error>, buffer: Option<Buffer>) -> Self;
}

impl<R> Output for Result<R, Error> {
    type Value = R;
    fn build(result: Result<R, Error>, _: Option<Buffer>) -> Self {
        result
    }
}

impl<R> Output for (Result<R, Error>, Buffer) {
    type Value = R;
    fn build(result: Result<R, Error>, buffer: Option<Buffer>) -> Self {
        (
            result,
            buffer.expect("an operation with a buffer returns it"),
        )
    }
}

/// An operation that has been asked for. Await it, or call [`Pending::wait`].
///
/// Dropping it does not stop the operation. The I/O thread lets it finish,
/// and only then takes back the buffer it holds, because the kernel may be
/// reading or writing that buffer until the operation ends.
#[must_use = "an operation's result, and the buffer it borrowed, come back through this"]
pub struct Pending<T: Output> {
    lane: Arc<Lane>,
    /// For an operation with no lent buffer: every lane, so that it is sent
    /// to the open one with the fewest in flight when it is sent.
    lanes: Option<Arc<[Arc<Lane>]>>,
    alive: Alive,
    stage: Stage,
    /// This operation's name while it waits for a slot.
    waiter: u64,
    /// The request, until it is sent.
    request: Option<Request>,
    /// The lent buffer the operation holds, as an index in the lane.
    buf: Option<u32>,
    map: fn(Finished) -> Result<T::Value, Error>,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    /// Waiting for an operation slot.
    Slot,
    /// Sent to the I/O thread under this slot.
    Sent(u32),
    /// Ended without being sent.
    Early(Error),
    Taken,
}

impl<T: Output> Pending<T> {
    pub(super) fn new(
        (lane, alive): (Arc<Lane>, Alive),
        key: &str,
        kind: Kind,
        buf: Option<u32>,
        len: u32,
        early: Option<Error>,
        map: fn(Finished) -> Result<T::Value, Error>,
    ) -> Self {
        let built = match early {
            Some(e) => Err(e),
            None => request(0, key, kind, buf.unwrap_or(0), len),
        };
        let (stage, request) = match built {
            Ok(r) => (Stage::Slot, Some(r)),
            Err(e) => (Stage::Early(e), None),
        };
        Pending {
            lane,
            lanes: None,
            alive,
            stage,
            waiter: waiter_id(),
            request,
            buf,
            map,
        }
    }

    /// Let the operation run on whichever of `lanes` is open when it is sent.
    pub(super) fn on_any(mut self, lanes: Arc<[Arc<Lane>]>) -> Self {
        self.lanes = Some(lanes);
        self
    }

    fn buffer(&mut self) -> Option<Buffer> {
        self.buf.take().map(|index| Buffer {
            lane: self.lane.clone(),
            index,
            alive: self.alive.clone(),
        })
    }

    /// Hand the request to the I/O thread under slot `op`. False, with the
    /// slot given back and the lane marked closed, if its thread is gone.
    fn send(&mut self, op: u32) -> bool {
        let mut r = self.request.take().expect("sent once");
        r.op = op;
        self.lane.cells[op as usize]
            .state
            .lock()
            .expect("cell lock")
            .buf = self.buf;
        self.lane.in_flight.fetch_add(1, Ordering::Relaxed);
        // The queue holds one request per slot, so it has room.
        if let Err(TrySendError::Full(r) | TrySendError::Disconnected(r)) =
            self.lane.requests.try_send(r)
        {
            self.request = Some(r);
            self.lane.in_flight.fetch_sub(1, Ordering::Relaxed);
            self.lane.cells[op as usize]
                .state
                .lock()
                .expect("cell lock")
                .buf = None;
            self.lane.ops.give(op);
            self.lane.close();
            return false;
        }
        self.stage = Stage::Sent(op);
        self.lane.wake.wake();
        true
    }

    /// Take the result of slot `op` and free the slot.
    fn take(&mut self, op: u32, finished: Finished) -> T {
        self.lane.ops.give(op);
        self.stage = Stage::Taken;
        let buffer = self.buffer();
        T::build((self.map)(finished), buffer)
    }

    fn early(&mut self, e: Error) -> T {
        self.stage = Stage::Taken;
        let buffer = self.buffer();
        T::build(Err(e), buffer)
    }

    /// While waiting for a slot: take one and send the request, or end
    /// closed if no lane it may use is open. Ready once the stage has moved on.
    fn poll_slot(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.stage != Stage::Slot {
            return Poll::Ready(());
        }
        loop {
            if let Some(lanes) = &self.lanes {
                let pick = least_busy(lanes);
                if !Arc::ptr_eq(pick, &self.lane) {
                    self.lane.ops.forget(self.waiter);
                    self.lane = pick.clone();
                }
            }
            if !open(&self.lane) {
                self.lane.ops.forget(self.waiter);
                self.stage = Stage::Early(Error::Closed);
                return Poll::Ready(());
            }
            match self.lane.ops.poll_take(self.waiter, cx) {
                Poll::Ready(op) if !open(&self.lane) => self.lane.ops.give(op),
                Poll::Ready(op) => {
                    if self.send(op) {
                        return Poll::Ready(());
                    }
                }
                Poll::Pending if open(&self.lane) => return Poll::Pending,
                Poll::Pending => {}
            }
        }
    }

    /// Block the calling thread until the operation ends.
    pub fn wait(mut self) -> T {
        block_on(core::future::poll_fn(|cx| self.poll_slot(cx)));
        match self.stage {
            Stage::Early(e) => self.early(e),
            Stage::Sent(op) => {
                let lane = self.lane.clone();
                let cell = &lane.cells[op as usize];
                let mut s = cell.state.lock().expect("cell lock");
                let finished = loop {
                    if let Some(f) = s.finished.take() {
                        break f;
                    }
                    s = cell.done.wait(s).expect("cell lock");
                };
                s.buf = None;
                drop(s);
                self.take(op, finished)
            }
            Stage::Slot | Stage::Taken => unreachable!("a result is taken once"),
        }
    }
}

impl<T: Output> Future for Pending<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        // Nothing in a `Pending` is self-referential.
        let me = unsafe { self.get_unchecked_mut() };
        if me.poll_slot(cx).is_pending() {
            return Poll::Pending;
        }
        match me.stage {
            Stage::Early(e) => Poll::Ready(me.early(e)),
            Stage::Sent(op) => {
                let lane = me.lane.clone();
                let mut s = lane.cells[op as usize].state.lock().expect("cell lock");
                match s.finished.take() {
                    Some(finished) => {
                        s.buf = None;
                        s.waker = None;
                        drop(s);
                        Poll::Ready(me.take(op, finished))
                    }
                    None => {
                        s.waker = Some(cx.waker().clone());
                        Poll::Pending
                    }
                }
            }
            Stage::Slot => unreachable!("a slot was taken or the lane closed"),
            Stage::Taken => panic!("polled after it returned its result"),
        }
    }
}

impl<T: Output> Drop for Pending<T> {
    fn drop(&mut self) {
        if self.stage == Stage::Slot {
            self.lane.ops.forget(self.waiter);
        }
        if let Stage::Sent(op) = self.stage {
            let mut s = self.lane.cells[op as usize]
                .state
                .lock()
                .expect("cell lock");
            if s.finished.take().is_some() {
                // It ended and nobody took the result: free the slot here.
                s.buf = None;
                s.waker = None;
                drop(s);
                self.lane.ops.give(op);
            } else {
                // Still running: the I/O thread frees the slot and the
                // buffer when it ends.
                s.detached = true;
                s.waker = None;
                self.buf = None;
            }
        }
        // A buffer this still holds goes back to its pool.
        drop(self.buffer());
    }
}
