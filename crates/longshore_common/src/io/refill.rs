// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The ring of returns of a receive area, and the queue behind it.
//!
//! A piece goes back to the kernel as one entry of a ring the kernel reads
//! when it wants to: lazily, and never more entries than the ring has. An
//! entry written past a full ring is not lost but read twice in place of
//! an older one, which takes a reference away from a piece that is still
//! out. So nothing is written while the ring is full: the return waits in
//! a queue of the backend's own, sized at build, and goes into the ring
//! when the kernel has read some of it, which a flush makes it do. The
//! queue does not grow: a return that finds it full too is told so, and
//! the backend has the kernel read the ring at once (a flush) and gives it
//! again, which then goes in.

use core::sync::atomic::{AtomicU32, Ordering};

/// One returned piece as the kernel reads it: `struct io_uring_zcrx_rqe`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Rqe {
    /// The offset in the area, with the area's token in the top bits.
    pub off: u64,
    /// The length, which the kernel does not read.
    pub len: u32,
    /// Zero.
    pub pad: u32,
}

/// The ring of returns and the queue behind it.
pub(crate) struct Refill {
    /// The kernel's count of entries read, and the program's of written.
    head: *const AtomicU32,
    tail: *const AtomicU32,
    rqes: *mut Rqe,
    /// Entries of the ring, a power of two.
    entries: u32,
    /// Entries written so far.
    written: u32,
    /// Returns that found the ring full, oldest first: a ring of its own,
    /// from `first`, `queued` of them.
    waiting: Box<[Rqe]>,
    first: usize,
    queued: usize,
    /// Goes into the top bits of every entry.
    token: u64,
    /// The kernel is to be asked to read the ring.
    flush: bool,
}

impl Refill {
    /// A ring of `entries` entries at `rqes`, with its counters at `head`
    /// and `tail`, and room for `waiting` returns behind it.
    ///
    /// # Safety
    /// `head`, `tail` and `entries` entries at `rqes` are valid for as long
    /// as this lives, and nothing else writes the tail or the entries.
    pub(crate) unsafe fn new(
        (head, tail): (*const AtomicU32, *const AtomicU32),
        rqes: *mut Rqe,
        entries: u32,
        waiting: usize,
        token: u64,
    ) -> Refill {
        Refill {
            head,
            tail,
            rqes,
            entries,
            written: unsafe { (*tail).load(Ordering::Relaxed) },
            waiting: vec![Rqe::default(); waiting.max(1)].into_boxed_slice(),
            first: 0,
            queued: 0,
            token,
            flush: false,
        }
    }

    /// Entries the ring has room for now.
    fn room(&self) -> u32 {
        let read = unsafe { (*self.head).load(Ordering::Acquire) };
        self.entries - self.written.wrapping_sub(read).min(self.entries)
    }

    /// Write `entry` into the ring, which has room.
    fn write(&mut self, entry: Rqe) {
        let at = (self.written & (self.entries - 1)) as usize;
        unsafe {
            self.rqes.add(at).write(entry);
            self.written = self.written.wrapping_add(1);
            (*self.tail).store(self.written, Ordering::Release);
        }
    }

    /// Return `len` bytes at `off` of the area. `false` when the ring and
    /// the queue behind it are both full: nothing was taken, and the
    /// kernel is to read the ring before the return is given again.
    #[must_use]
    pub(crate) fn give(&mut self, off: u64, len: u32) -> bool {
        let entry = Rqe {
            off: off | self.token,
            len,
            pad: 0,
        };
        // Behind what waits already, so that none waits for ever.
        if self.queued == 0 && self.room() > 0 {
            self.write(entry);
            return true;
        }
        self.flush = true;
        if self.queued == self.waiting.len() {
            return false;
        }
        let at = (self.first + self.queued) % self.waiting.len();
        self.waiting[at] = entry;
        self.queued += 1;
        true
    }

    /// Move what waits into the ring, as far as it has room.
    pub(crate) fn drain(&mut self) {
        while self.queued > 0 && self.room() > 0 {
            let entry = self.waiting[self.first];
            self.first = (self.first + 1) % self.waiting.len();
            self.queued -= 1;
            self.write(entry);
        }
    }

    /// The kernel is to read the ring before the next receive is armed:
    /// what it copies into comes back to it only then.
    pub(crate) fn ask_flush(&mut self) {
        self.flush = true;
    }

    /// Whether the kernel is to be asked to read the ring now.
    pub(crate) fn wants_flush(&self) -> bool {
        self.flush
    }

    /// The kernel has read the ring: what waited goes in, and it is asked
    /// again if some still waits.
    pub(crate) fn flushed(&mut self) {
        self.drain();
        self.flush = self.queued > 0;
    }

    /// Returns that wait behind the ring.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.queued
    }
}

#[cfg(test)]
mod tests;
