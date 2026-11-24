// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How much of everything a `Net` gets.

/// What to add to the backend for a `Net`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sizes {
    pub(crate) connects: u32,
    pub(crate) accepts: u32,
    pub(crate) listeners: u32,
    pub(crate) frames: u32,
    pub(crate) frame_size: u32,
    pub(crate) segments: u32,
    pub(crate) segment_size: u32,
    pub(crate) ring_buffers: u32,
    pub(crate) ring_buffer_size: u32,
    pub(crate) header_max: u32,
    pub(crate) spans: u32,
    pub(crate) zero_copy_min: u32,
    pub(crate) token_base: u64,
    pub(crate) seed: u64,
}

impl Sizes {
    /// `connects` connections the `Net` opens itself (and those a listener
    /// with peer addresses accepts), `accepts` connections its other
    /// listeners accept, `listeners` listeners. Defaults for the rest: 64
    /// frames of 1 MiB, 64 send segments of 64 KiB, 64 ring buffers of
    /// 64 KiB, headers up to 256 bytes, 16 queued sends per connection,
    /// zero-copy sends from 16 KiB, tokens from 0.
    pub const fn new(connects: u32, accepts: u32, listeners: u32) -> Sizes {
        Sizes {
            connects,
            accepts,
            listeners,
            frames: 64,
            frame_size: 1 << 20,
            segments: 64,
            segment_size: 64 << 10,
            ring_buffers: 64,
            ring_buffer_size: 64 << 10,
            header_max: 256,
            spans: 16,
            zero_copy_min: 16 << 10,
            token_base: 0,
            seed: 0x6c6f_6e67_7368_6f72,
        }
    }

    /// `count` buffers of frame memory, each `size` bytes: the longest
    /// frame, and a multiple of the backend's block size.
    #[must_use]
    pub const fn with_frames(mut self, count: u32, size: u32) -> Sizes {
        (self.frames, self.frame_size) = (count, size);
        self
    }

    /// `count` send segments of `size` bytes: the longest frame the caller
    /// serialises with `send_frame`.
    #[must_use]
    pub const fn with_segments(mut self, count: u32, size: u32) -> Sizes {
        (self.segments, self.segment_size) = (count, size);
        self
    }

    /// `count` buffers of `size` bytes in the backend's buffer ring, shared
    /// with whatever else on the backend uses armed receives.
    #[must_use]
    pub const fn with_ring(mut self, count: u32, size: u32) -> Sizes {
        (self.ring_buffers, self.ring_buffer_size) = (count, size);
        self
    }

    /// The longest frame header of any framing used.
    #[must_use]
    pub const fn with_header_max(mut self, bytes: u32) -> Sizes {
        self.header_max = bytes;
        self
    }

    /// Sends that can wait in one connection's queue.
    #[must_use]
    pub const fn with_spans(mut self, per_connection: u32) -> Sizes {
        self.spans = per_connection;
        self
    }

    /// Send without a kernel copy from this many bytes up; `u32::MAX` never.
    #[must_use]
    pub const fn with_zero_copy_min(mut self, bytes: u32) -> Sizes {
        self.zero_copy_min = bytes;
        self
    }

    /// The first of the `Net`'s tokens; it uses [`crate::Config::tokens`] of them.
    #[must_use]
    pub const fn with_token_base(mut self, base: u64) -> Sizes {
        self.token_base = base;
        self
    }

    /// Seed for reconnect jitter.
    #[must_use]
    pub const fn with_seed(mut self, seed: u64) -> Sizes {
        self.seed = seed;
        self
    }
}
