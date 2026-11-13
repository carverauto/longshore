// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Lent buffers, and waiting for one.

use super::lane::{waiter_id, Lane};
use super::{open, Alive};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// A registered buffer on loan from a client. Dropping it returns it.
///
/// While an operation holds a buffer the caller cannot reach it: the
/// operation takes the buffer and gives it back with its result.
pub struct Buffer {
    pub(super) lane: Arc<Lane>,
    pub(super) index: u32,
    /// Keeps the I/O threads, and so this buffer's memory, alive.
    pub(super) alive: Alive,
}

impl Buffer {
    /// The whole buffer.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // The index is held by this guard alone, and no operation holds it.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.lane.base.add(self.index as usize * self.lane.buf_size),
                self.lane.buf_size,
            )
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self.lane.base.add(self.index as usize * self.lane.buf_size),
                self.lane.buf_size,
            )
        }
    }

    pub fn len(&self) -> usize {
        self.lane.buf_size
    }

    pub fn is_empty(&self) -> bool {
        self.lane.buf_size == 0
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
        self.lane.bufs.give(self.index);
    }
}

/// Waits for a buffer on every open lane at once, and takes the first that
/// is free. Only when every lane is closed does it take a closed lane's.
pub(super) struct Borrow {
    lanes: Arc<[Arc<Lane>]>,
    alive: Alive,
    waiter: u64,
    /// Waiting in some lane's queue.
    queued: bool,
}

impl Borrow {
    pub(super) fn new(lanes: Arc<[Arc<Lane>]>, alive: Alive) -> Borrow {
        Borrow {
            lanes,
            alive,
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
                l.bufs.forget(self.waiter);
            }
        }
    }
}

impl Future for Borrow {
    type Output = Buffer;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Buffer> {
        let me = self.get_mut();
        loop {
            let closed = me.closed();
            let all_closed = closed == me.lanes.len();
            for n in 0..me.lanes.len() {
                let lane = &me.lanes[n];
                if !all_closed && !open(lane) {
                    continue;
                }
                if let Poll::Ready(index) = lane.bufs.poll_take(me.waiter, cx) {
                    let lane = lane.clone();
                    me.forget();
                    return Poll::Ready(Buffer {
                        lane,
                        index,
                        alive: me.alive.clone(),
                    });
                }
                me.queued = true;
            }
            if me.closed() == closed {
                return Poll::Pending;
            }
        }
    }
}

impl Drop for Borrow {
    fn drop(&mut self) {
        self.forget();
    }
}
