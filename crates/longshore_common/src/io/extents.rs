// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Buffers shared by several holders, each until it is done with them.
//!
//! A frame received from the network is written to a log and sent on to
//! peers from the buffer it was received into. The buffer must then outlive
//! the receive: the log holds it until the write is durable, each send
//! queue until its send completes. [`Extents`] keeps a hold count and a
//! generation per buffer of a range of the pool. One thread owns it, so
//! both are plain integers. A buffer returns to the free list when its last
//! hold is released, and its generation rises, so an [`Extent`] kept past
//! that point is refused instead of reaching the buffer's next user.
//!
//! The range is normally of the block-aligned class
//! ([`crate::io::Limits::aligned`]), which sockets can receive into and send
//! from and direct-I/O file operations require.

use super::BufId;

/// One buffer of an [`Extents`] range, as it was when it was taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    buf: BufId,
    generation: u32,
}

impl Extent {
    /// The pool buffer. Valid while the holder has not released its hold.
    #[inline]
    pub const fn buf(self) -> BufId {
        self.buf
    }
}

/// Why an operation on an [`Extent`] was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtentError {
    /// The buffer has been freed since this extent was taken, or the extent
    /// is not of this range.
    Stale,
    /// The buffer already has as many holds as can be counted (65,535).
    TooManyHolds,
}

/// Hold counts and generations for `count` pool buffers starting at `first`.
pub struct Extents {
    first: u32,
    holds: Box<[u16]>,
    generations: Box<[u32]>,
    /// Indexes of free buffers; the top `free_len` entries are a stack.
    free: Box<[u32]>,
    free_len: usize,
}

impl Extents {
    /// Every buffer free. Allocates its tables once, here.
    pub fn new(first: BufId, count: u32) -> Extents {
        Extents {
            first: first.0,
            holds: vec![0; count as usize].into_boxed_slice(),
            generations: vec![0; count as usize].into_boxed_slice(),
            free: (0..count).rev().collect(),
            free_len: count as usize,
        }
    }

    /// A free buffer with one hold on it, or `None` when all are held.
    pub fn take(&mut self) -> Option<Extent> {
        let top = self.free_len.checked_sub(1)?;
        self.free_len = top;
        let i = self.free[top] as usize;
        self.holds[i] = 1;
        Some(Extent {
            buf: BufId(self.first + i as u32),
            generation: self.generations[i],
        })
    }

    /// The index of a live extent of this range.
    fn index(&self, e: Extent) -> Result<usize, ExtentError> {
        let i = e.buf.0.checked_sub(self.first).ok_or(ExtentError::Stale)? as usize;
        let live = self.generations.get(i) == Some(&e.generation) && self.holds[i] > 0;
        if live {
            Ok(i)
        } else {
            Err(ExtentError::Stale)
        }
    }

    /// One more hold: the buffer stays until this one is released too.
    pub fn hold(&mut self, e: Extent) -> Result<(), ExtentError> {
        let i = self.index(e)?;
        self.holds[i] = self.holds[i]
            .checked_add(1)
            .ok_or(ExtentError::TooManyHolds)?;
        Ok(())
    }

    /// Release one hold. `Ok(true)` when it was the last and the buffer is
    /// free again; every copy of `e` is stale from then on.
    pub fn release(&mut self, e: Extent) -> Result<bool, ExtentError> {
        let i = self.index(e)?;
        self.holds[i] -= 1;
        if self.holds[i] > 0 {
            return Ok(false);
        }
        self.generations[i] = self.generations[i].wrapping_add(1);
        self.free[self.free_len] = i as u32;
        self.free_len += 1;
        Ok(true)
    }

    /// Holds on a live extent; `None` for a stale one.
    pub fn holds(&self, e: Extent) -> Option<u16> {
        self.index(e).ok().map(|i| self.holds[i])
    }

    /// Whether `e` still names the buffer it was taken as.
    pub fn is_live(&self, e: Extent) -> bool {
        self.index(e).is_ok()
    }

    /// Buffers nobody holds.
    pub fn free_count(&self) -> usize {
        self.free_len
    }

    /// Buffers in the range.
    pub fn count(&self) -> usize {
        self.holds.len()
    }
}
