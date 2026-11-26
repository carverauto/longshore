// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How much of everything a `Net` gets.

/// The smallest segment a `Net` with TLS can have: a connection's TLS
/// buffer holds two records of the largest size.
pub const TLS_SEGMENT_MIN: u32 = 36 << 10;

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
    pub(crate) zero_copy_sends: u32,
    pub(crate) zero_copy_sends_in_all: u32,
    pub(crate) tls: bool,
    pub(crate) token_base: u64,
    pub(crate) seed: u64,
}

impl Sizes {
    /// `connects` connections the `Net` opens itself (and those a listener
    /// with peer addresses accepts), `accepts` connections its other
    /// listeners accept, `listeners` listeners. Defaults for the rest: 64
    /// frames of 1 MiB, 64 send segments of 64 KiB, 64 ring buffers of
    /// 16 KiB, headers up to 256 bytes, 16 queued sends per connection,
    /// zero-copy sends from 16 KiB, one of them a connection and 256 in
    /// all whose memory the kernel may still read, tokens from 0.
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
            ring_buffer_size: 16 << 10,
            header_max: 256,
            spans: 16,
            zero_copy_min: 16 << 10,
            zero_copy_sends: 4,
            zero_copy_sends_in_all: 256,
            tls: false,
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

    /// Zero-copy sends whose memory the kernel still reads: how many a
    /// connection may have, and how many all connections together. A
    /// zero-copy send is reported as soon as its bytes are taken, so that
    /// the next follows at once; its memory comes back when the peer has
    /// acknowledged the bytes. With `per_connection` of its own out, or
    /// `in_all` out in the set, a connection sends with a copy until one
    /// comes back. Each is an operation of the backend until then, and
    /// `in_all` (or `per_connection` for every connection, if that is
    /// fewer) is what the set adds to the backend's operations for them.
    /// With `per_connection` 0 a connection has one zero-copy send at a
    /// time and waits for its memory before the next: a send every round
    /// trip. The default of four is from runs between two machines over
    /// a 200 Gb/s card (`bench/results/m32-tcp-zero-copy-send-dgx-spark-pair.md`):
    /// four carried as much as any number tried and about halved the
    /// sending thread's CPU against one, with which most sends are
    /// copied; eight was slower in two cases of nine. Nothing is lent
    /// to a loopback address or a Unix-domain socket, where the kernel
    /// copies whatever it is asked.
    #[must_use]
    pub const fn with_zero_copy_sends(mut self, per_connection: u32, in_all: u32) -> Sizes {
        (self.zero_copy_sends, self.zero_copy_sends_in_all) = (per_connection, in_all);
        self
    }

    /// The most zero-copy sends a set of `conns` connections has whose
    /// memory is still read.
    pub(crate) const fn lent_most(&self, conns: u32) -> u32 {
        let each = conns.saturating_mul(self.zero_copy_sends);
        if each < self.zero_copy_sends_in_all {
            each
        } else {
            self.zero_copy_sends_in_all
        }
    }

    /// Room for TLS: two more buffers of the segment size for every
    /// connection, which hold its records on the way out and in. Segments
    /// must then be at least [`TLS_SEGMENT_MIN`] bytes.
    #[must_use]
    pub const fn with_tls(mut self, on: bool) -> Sizes {
        self.tls = on;
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
