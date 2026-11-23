// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! An operation in progress: a future, with a blocking wait as well.

use super::buffer::NO_INDEX;
use super::lane::{Finished, Kind, Lane, Request};
use super::{least_busy, open, request, Alive, Buffer, Error};
use longshore_mailbox::{block_on, SlotWaiter, TrySendError};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
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
    /// For an operation with no lent buffer: lanes to wait across, starting at
    /// the open one with the fewest in flight.
    any: Option<Arc<[Arc<Lane>]>>,
    alive: Alive,
    stage: Stage,
    /// This operation's waiter state while it waits for a slot.
    waiter: SlotWaiter,
    /// The request, until it is sent.
    request: Option<Request>,
    /// The lent buffer the operation holds, as an index in the lane.
    buf: Option<u32>,
    /// The second argument is the part number, for a part of an upload.
    map: fn(&Finished, u32) -> Result<T::Value, Error>,
    number: u32,
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
        map: fn(&Finished, u32) -> Result<T::Value, Error>,
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
            any: None,
            alive,
            stage,
            waiter: SlotWaiter::new(),
            request,
            buf,
            map,
            number: 0,
        }
    }

    /// Name the multipart upload the request belongs to, and its part.
    pub(super) fn of_upload(mut self, upload: &crate::multipart::UploadId, number: u32) -> Self {
        if let Some(r) = &mut self.request {
            r.upload = Some(*upload);
            r.number = number;
        }
        self.number = number;
        self
    }

    /// Give the request its limits and the object it must find.
    pub(super) fn within(mut self, budget: super::Budget, expect: &crate::report::Expect) -> Self {
        if let Some(r) = &mut self.request {
            r.budget = budget;
            r.expect = *expect;
        }
        self
    }

    /// Let the operation run on whichever of `lanes` is open when it is sent.
    pub(super) fn on_any(mut self, lanes: Arc<[Arc<Lane>]>) -> Self {
        self.any = Some(lanes);
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
    /// slot given back, if its thread is closed.
    fn send(&mut self, op: u32) -> bool {
        let mut r = self.request.take().expect("sent once");
        r.op = op;
        self.lane.in_flight.fetch_add(1, Ordering::Relaxed);
        let tag = self.buf.filter(|&b| b != NO_INDEX);
        match self.lane.sender.send_slot(op, tag, r) {
            Ok(()) => {
                self.stage = Stage::Sent(op);
                true
            }
            Err(TrySendError::Closed(r)) => {
                self.request = Some(r);
                self.lane.in_flight.fetch_sub(1, Ordering::Relaxed);
                self.lane.sender.pool().give(op);
                false
            }
            Err(TrySendError::Full(_)) => {
                self.lane.in_flight.fetch_sub(1, Ordering::Relaxed);
                self.lane.sender.pool().give(op);
                panic!("mailbox queue full while holding an operation slot");
            }
        }
    }

    fn finish(&mut self, finished: &Finished) -> T {
        self.stage = Stage::Taken;
        let buffer = self.buffer();
        T::build((self.map)(finished, self.number), buffer)
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
            let op = match &self.any {
                Some(lanes) => {
                    let first = least_busy(lanes);
                    match core::task::ready!(self.waiter.poll_many(
                        lanes,
                        first,
                        |l| l.sender.pool(),
                        |l| open(l),
                        false,
                        cx,
                    )) {
                        Some((idx, op)) => {
                            self.lane = lanes[idx].clone();
                            op
                        }
                        None => {
                            self.stage = Stage::Early(Error::Closed);
                            return Poll::Ready(());
                        }
                    }
                }
                None => match core::task::ready!(self.waiter.poll_one(
                    self.lane.sender.pool(),
                    || open(&self.lane),
                    cx,
                )) {
                    Some(op) => op,
                    None => {
                        self.stage = Stage::Early(Error::Closed);
                        return Poll::Ready(());
                    }
                },
            };
            if self.send(op) {
                return Poll::Ready(());
            }
            if self.any.is_none() {
                self.stage = Stage::Early(Error::Closed);
                return Poll::Ready(());
            }
        }
    }

    /// Block the calling thread until the operation ends.
    pub fn wait(mut self) -> T {
        block_on(core::future::poll_fn(|cx| self.poll_slot(cx)));
        match self.stage {
            Stage::Early(e) => self.early(e),
            Stage::Sent(op) => {
                let finished = self.lane.sender.wait_answer(op);
                self.finish(&finished)
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
            Stage::Sent(op) => match me.lane.sender.poll_answer(op, cx) {
                Poll::Ready(finished) => Poll::Ready(me.finish(&finished)),
                Poll::Pending => Poll::Pending,
            },
            Stage::Slot => unreachable!("a slot was taken or the lane closed"),
            Stage::Taken => panic!("polled after it returned its result"),
        }
    }
}

impl<T: Output> Drop for Pending<T> {
    fn drop(&mut self) {
        if self.stage == Stage::Slot {
            match &self.any {
                Some(lanes) => self.waiter.forget_many(lanes, |l| l.sender.pool()),
                None => self.waiter.forget_one(self.lane.sender.pool()),
            }
        }
        if let Stage::Sent(op) = self.stage {
            if self.lane.sender.detach(op).is_none() {
                // Still running: the I/O thread frees the slot first and then
                // returns the buffer tag to the lane's pool.
                self.buf = None;
            }
        }
        // A buffer this still holds goes back to its pool.
        drop(self.buffer());
    }
}
