// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Lent buffers, and waiting for one.

use super::lane::{waiter_id, Lane, Pool};
use super::{open, Alive};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// A registered buffer on loan from a client. Dropping it returns it.
///
/// While an operation holds a buffer the caller cannot reach it: the
/// operation takes the buffer and gives it back with its result.
///
/// A buffer is void if its I/O thread failed and the kernel could not be
/// shown to have stopped using that thread's buffers, or if it was lent
/// when every I/O thread had stopped and none was free. A void buffer is
/// empty, so no memory can be reached through it, dropping it returns
/// nothing, and an operation given it ends as [`super::Error::Closed`].
pub struct Buffer {
    pub(super) lane: Arc<Lane>,
    pub(super) index: u32,
    /// Keeps the I/O threads, and so this buffer's memory, alive.
    pub(super) alive: Alive,
}

/// The index of a void buffer lent when none was free.
pub(super) const NO_INDEX: u32 = u32::MAX;

impl Buffer {
    /// The whole buffer.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        if self.is_void() {
            return &mut [];
        }
        // The index is held by this guard alone, and no operation holds it.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.lane.base.add(self.index as usize * self.lane.buf_size),
                self.lane.buf_size,
            )
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.is_void() {
            return &[];
        }
        unsafe {
            core::slice::from_raw_parts(
                self.lane.base.add(self.index as usize * self.lane.buf_size),
                self.lane.buf_size,
            )
        }
    }

    /// The buffer's size, or zero if it is void.
    pub fn len(&self) -> usize {
        if self.is_void() {
            0
        } else {
            self.lane.buf_size
        }
    }

    pub(super) fn is_void(&self) -> bool {
        self.index == NO_INDEX || self.lane.is_void()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Give the index to an operation without returning it to the pool.
    pub(super) fn into_parts(self) -> (Arc<Lane>, u32, Alive) {
        let me = core::mem::ManuallyDrop::new(self);
        // Move the fields out of a value whose `Drop` will not run.
        unsafe {
            (
                core::ptr::read(&me.lane),
                me.index,
                core::ptr::read(&me.alive),
            )
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if self.index != NO_INDEX {
            self.lane.give_buffer(self.index);
        }
    }
}

/// Waits on one pool of every open lane at once, and takes the first index
/// that is free, looking first at lane `first`. Only when every lane is
/// closed does it take a closed lane's, and if none is free then, it ends
/// with nothing instead of waiting.
pub(super) struct AnyLane {
    pub(super) lanes: Arc<[Arc<Lane>]>,
    pool: fn(&Lane) -> &Pool,
    waiter: u64,
    /// Waiting in some lane's queue.
    queued: bool,
}

impl AnyLane {
    pub(super) fn new(lanes: Arc<[Arc<Lane>]>, pool: fn(&Lane) -> &Pool) -> AnyLane {
        AnyLane {
            lanes,
            pool,
            waiter: waiter_id(),
            queued: false,
        }
    }

    fn closed(&self) -> usize {
        self.lanes.iter().filter(|l| !open(l)).count()
    }

    fn forget(&mut self) {
        if self.queued {
            self.queued = false;
            for l in self.lanes.iter() {
                (self.pool)(l).forget(self.waiter);
            }
        }
    }

    pub(super) fn poll(
        &mut self,
        first: usize,
        cx: &mut Context<'_>,
    ) -> Poll<Option<(Arc<Lane>, u32)>> {
        let count = self.lanes.len();
        loop {
            let closed = self.closed();
            let all_closed = closed == count;
            for n in 0..count {
                let lane = &self.lanes[(first + n) % count];
                if !all_closed && !open(lane) {
                    continue;
                }
                let pool = (self.pool)(lane);
                if let Poll::Ready(index) = pool.poll_take(self.waiter, cx) {
                    if !all_closed && !open(lane) {
                        pool.give(index);
                        continue;
                    }
                    let lane = lane.clone();
                    self.forget();
                    return Poll::Ready(Some((lane, index)));
                }
                self.queued = true;
            }
            if self.closed() == closed {
                if !all_closed {
                    return Poll::Pending;
                }
                self.forget();
                return Poll::Ready(None);
            }
        }
    }
}

impl Drop for AnyLane {
    fn drop(&mut self) {
        self.forget();
    }
}

/// Waits for a buffer on every lane: see [`AnyLane`]. If no lane is open
/// and none is free, it ends with a void buffer.
pub(super) struct Borrow {
    any: AnyLane,
    alive: Alive,
}

impl Borrow {
    pub(super) fn new(lanes: Arc<[Arc<Lane>]>, alive: Alive) -> Borrow {
        Borrow {
            any: AnyLane::new(lanes, |l| &l.bufs),
            alive,
        }
    }
}

impl Future for Borrow {
    type Output = Buffer;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Buffer> {
        let me = self.get_mut();
        let (lane, index) = match core::task::ready!(me.any.poll(0, cx)) {
            Some(taken) => taken,
            None => (me.any.lanes[0].clone(), NO_INDEX),
        };
        Poll::Ready(Buffer {
            lane,
            index,
            alive: me.alive.clone(),
        })
    }
}
