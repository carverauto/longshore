// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a `Net` holds at a moment: for tests and for a caller that wants
//! to know before it shuts down.

use super::state::{LSt, St};
use super::Net;
use crate::transport::Transport;

/// What a `Net` holds. With no connection, no listener and no frame held
/// by the caller, every pool is full and no deadline is armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Usage {
    connections: u32,
    listeners: u32,
    frames_free: u32,
    frames: u32,
    segments_free: u32,
    segments: u32,
    ring_buffers_kept: u32,
    deadline: bool,
    zc: Paged,
}

/// What a `Net` that receives in pages (`Rx::ZeroCopy`) holds, and has
/// done since it was built: [`Usage::pages`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Paged {
    pages: (u32, u32),
    runs: u32,
    pieces: u32,
    frames_kept: u64,
    frames_moved: u64,
    moved_bytes: u64,
    area_short: u64,
    rearmed: u64,
}

impl Paged {
    /// Pages frames in pages hold, arriving or lent, and the budget.
    pub const fn pages(&self) -> (u32, u32) {
        self.pages
    }

    /// Runs in use: frames in pages arriving or lent.
    pub const fn runs(&self) -> u32 {
        self.runs
    }

    /// Pieces of the receive area the set holds or has still to give
    /// back: in runs, in headers kept for a decision, waiting.
    pub const fn pieces(&self) -> u32 {
        self.pieces
    }

    /// Frames lent in their pages.
    pub const fn frames_kept(&self) -> u64 {
        self.frames_kept
    }

    /// Frames of the mode moved into frame memory instead.
    pub const fn frames_moved(&self) -> u64 {
        self.frames_moved
    }

    /// Bytes the set copied on receive: a kept frame's header and last
    /// block, a moved frame whole.
    pub const fn moved_bytes(&self) -> u64 {
        self.moved_bytes
    }

    /// Receives that ended for want of a page for the kernel's copy: the
    /// copy path found no page, not that the area is empty.
    pub const fn area_short(&self) -> u64 {
        self.area_short
    }

    /// Receives that ended for want of room to report in, and were armed
    /// again.
    pub const fn rearmed(&self) -> u64 {
        self.rearmed
    }
}

impl Usage {
    /// Connection slots not free: connections in any state, those being
    /// closed and those waiting to dial again included.
    pub const fn connections(&self) -> u32 {
        self.connections
    }

    /// Listener slots not free.
    pub const fn listeners(&self) -> u32 {
        self.listeners
    }

    /// Frame buffers free, and in all.
    pub const fn frames(&self) -> (u32, u32) {
        (self.frames_free, self.frames)
    }

    /// Send segments free, and in all.
    pub const fn segments(&self) -> (u32, u32) {
        (self.segments_free, self.segments)
    }

    /// Ring buffers the `Net` has not given back yet.
    pub const fn ring_buffers_kept(&self) -> u32 {
        self.ring_buffers_kept
    }

    /// Whether a deadline is armed.
    pub const fn deadline(&self) -> bool {
        self.deadline
    }

    /// What it holds of the receive area, and what it did with frames
    /// of the mode.
    pub const fn pages(&self) -> Paged {
        self.zc
    }

    /// Whether nothing is held: no connection, no listener, every pool
    /// full, no piece of the receive area, no deadline.
    pub const fn is_idle(&self) -> bool {
        self.connections == 0
            && self.listeners == 0
            && self.frames_free == self.frames
            && self.segments_free == self.segments
            && self.ring_buffers_kept == 0
            && self.zc.runs == 0
            && self.zc.pieces == 0
            && !self.deadline
    }
}

impl<T: Transport> Net<T> {
    /// What the `Net` holds now.
    pub fn usage(&mut self) -> Usage {
        let open = |st: &St| *st != St::Free;
        let kept = self.chunks_kept() + self.returns_len as u32;
        Usage {
            connections: self.conns.iter().filter(|c| open(&c.st)).count() as u32,
            listeners: self.listeners.iter().filter(|l| l.st != LSt::Free).count() as u32,
            frames_free: self.extents.free_count() as u32,
            frames: self.extents.count() as u32,
            segments_free: self.segs_len as u32,
            segments: self.cfg.sizes.segments,
            ring_buffers_kept: kept,
            deadline: self.next_deadline().is_some(),
            zc: self.paged(),
        }
    }

    fn paged(&self) -> Paged {
        let z = &self.zc;
        let (runs, records) = z.in_use();
        Paged {
            pages: z.kept(),
            runs,
            pieces: records + z.headers + z.waiting(),
            frames_kept: z.frames_kept,
            frames_moved: z.frames_moved,
            moved_bytes: z.moved_bytes,
            area_short: z.area_short,
            rearmed: z.rearmed,
        }
    }

    /// Ring buffers still queued on connections.
    fn chunks_kept(&self) -> u32 {
        let mut kept = 0;
        for c in self.conns.iter() {
            let mut i = c.chunk_head;
            while i != super::state::NONE {
                kept += 1;
                i = self.chunks[i as usize].next;
            }
        }
        kept
    }
}
