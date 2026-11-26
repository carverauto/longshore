// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Pool sizing and buffer layout configuration ([`Limits`]).

use crate::io::BufId;

mod fields;

/// Everything the I/O layer allocates is sized from these, once.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub(crate) sockets_max: u32,
    pub(crate) accept_range_start: u32,
    pub(crate) accept_range_len: u32,
    pub(crate) buffers: u32,
    pub(crate) buffer_size: u32,
    pub(crate) small_buffers: u32,
    pub(crate) small_buffer_size: u32,
    pub(crate) files_max: u32,
    pub(crate) aligned_buffers: u32,
    pub(crate) aligned_buffer_size: u32,
    pub(crate) block_size: u32,
    pub(crate) ring_buffers: u32,
    pub(crate) ring_buffer_size: u32,
    pub(crate) area_pages: u32,
    pub(crate) area_pieces: u32,
    pub(crate) area_interface: u32,
    pub(crate) area_queue: u32,
    pub(crate) gather_ops: u32,
    pub(crate) gather_spans: u32,
    pub(crate) ops_max: u32,
}

impl Limits {
    /// The zeroed value every `Limits` is built from with the `with_*`
    /// setters: `Limits::BASE.with_sockets_max(4).with_buffers(3)`.
    pub const BASE: Self = Self {
        sockets_max: 0,
        accept_range_start: 0,
        accept_range_len: 0,
        buffers: 0,
        buffer_size: 0,
        small_buffers: 0,
        small_buffer_size: 0,
        files_max: 0,
        aligned_buffers: 0,
        aligned_buffer_size: 0,
        block_size: 4096,
        ring_buffers: 0,
        ring_buffer_size: 0,
        area_pages: 0,
        area_pieces: 0,
        area_interface: 0,
        area_queue: 0,
        gather_ops: 0,
        gather_spans: 0,
        ops_max: 0,
    };

    /// Bytes in a page of the receive area: what a network card that
    /// writes into it grants, and no other size.
    pub const PAGE: u32 = 4096;

    /// The most pieces of the receive area that can be out at once: what
    /// a kernel's ring of returns holds.
    pub const AREA_PIECES_MOST: u32 = 32_768;

    /// The most spans one gathered write can have: what a kernel takes in
    /// one vector.
    pub const GATHER_SPANS_MOST: u32 = 1024;

    /// Validate alignment, buffer-ring, and accept-range invariants.
    pub fn validate(&self) {
        if self.aligned_buffers > 0 {
            assert!(
                self.block_size >= 4096 && self.block_size.is_power_of_two(),
                "block_size must be a power of two >= 4096 when aligned_buffers > 0"
            );
            assert!(
                self.aligned_buffer_size > 0
                    && self.aligned_buffer_size.is_multiple_of(self.block_size),
                "aligned_buffer_size must be a non-zero multiple of block_size"
            );
        } else if self.block_size != 0 {
            assert!(
                self.block_size >= 4096 && self.block_size.is_power_of_two(),
                "block_size must be 0 or a power of two >= 4096"
            );
        }
        assert!(
            self.ring_buffers <= 32_768,
            "ring_buffers must be <= 32_768"
        );
        if self.ring_buffers > 0 {
            assert!(
                self.ring_buffer_size > 0,
                "ring_buffer_size must be > 0 when ring_buffers > 0"
            );
        }
        assert!(
            self.area_pieces <= Self::AREA_PIECES_MOST,
            "area_pieces must be <= 32_768"
        );
        assert!(
            (self.gather_ops == 0) == (self.gather_spans == 0),
            "gather_ops and gather_spans are both zero or neither"
        );
        assert!(
            self.gather_spans <= Self::GATHER_SPANS_MOST,
            "gather_spans must be <= 1024"
        );
        if self.accept_range_len > 0 {
            assert!(
                self.accept_range_start
                    .checked_add(self.accept_range_len)
                    .is_some_and(|end| end <= self.sockets_max),
                "accept_range_start + accept_range_len must be <= sockets_max"
            );
        }
        assert!(
            self.buffers
                .checked_add(self.small_buffers)
                .and_then(|n| n.checked_add(self.aligned_buffers))
                .and_then(|n| n.checked_add(self.ring_buffers))
                .and_then(|n| n.checked_add(self.area_pages))
                .is_some(),
            "total buffer count overflows u32"
        );
    }

    /// First buffer id of the buffer-ring class.
    pub fn ring_base(&self) -> u32 {
        self.buffers + self.small_buffers + self.aligned_buffers
    }

    /// The pieces of the receive area a backend makes room to have
    /// returned at once: [`Limits::area_pieces`], or, when that was left
    /// at zero, a piece a page up to [`Limits::AREA_PIECES_MOST`]; a power
    /// of two, as a kernel's ring of returns is.
    pub fn area_returns(&self) -> u32 {
        let asked = if self.area_pieces == 0 {
            self.area_pages
        } else {
            self.area_pieces
        };
        asked.clamp(64, Self::AREA_PIECES_MOST).next_power_of_two()
    }

    /// First buffer id of the receive area's pages.
    pub fn area_base(&self) -> u32 {
        self.ring_base() + self.ring_buffers
    }

    /// Total buffers across all five classes.
    pub fn buffer_count(&self) -> u32 {
        self.area_base() + self.area_pages
    }

    /// The id of page `i` of the receive area.
    pub fn page(&self, i: u32) -> BufId {
        assert!(i < self.area_pages, "page out of range");
        BufId(self.area_base() + i)
    }

    /// Whether `id` is a page of the receive area.
    pub fn is_page(&self, id: BufId) -> bool {
        id.0 >= self.area_base() && id.0 < self.buffer_count()
    }

    /// The id of small buffer `i`.
    pub fn small(&self, i: u32) -> BufId {
        assert!(i < self.small_buffers, "small buffer out of range");
        BufId(self.buffers + i)
    }

    /// The id of aligned buffer `i`.
    pub fn aligned(&self, i: u32) -> BufId {
        self.validate();
        assert!(i < self.aligned_buffers, "aligned buffer out of range");
        BufId(self.buffers + self.small_buffers + i)
    }

    /// The id of ring buffer `i`.
    pub fn ring(&self, i: u32) -> BufId {
        self.validate();
        assert!(i < self.ring_buffers, "ring buffer out of range");
        BufId(self.ring_base() + i)
    }

    /// Whether `id` belongs to the block-aligned buffer class.
    pub fn is_aligned_buf(&self, id: BufId) -> bool {
        let start = self.buffers + self.small_buffers;
        id.0 >= start && id.0 < start + self.aligned_buffers
    }

    /// Whether `id` belongs to the buffer-ring buffer class.
    pub fn is_ring_buf(&self, id: BufId) -> bool {
        let start = self.ring_base();
        id.0 >= start && id.0 < start + self.ring_buffers
    }

    fn unaligned_bytes(&self) -> usize {
        self.buffers as usize * self.buffer_size as usize
            + self.small_buffers as usize * self.small_buffer_size as usize
    }

    /// Byte offset inside the aligned pool view where the aligned class begins.
    pub fn aligned_start(&self) -> usize {
        let u = self.unaligned_bytes();
        if self.aligned_buffers == 0 {
            u
        } else {
            u.next_multiple_of(self.block_size as usize)
        }
    }

    /// Byte offset inside the aligned pool view where the buffer-ring class begins.
    pub fn ring_start(&self) -> usize {
        self.aligned_start() + self.aligned_buffers as usize * self.aligned_buffer_size as usize
    }

    /// Byte offset inside the aligned pool view where the receive area
    /// begins: at a page boundary, as a card needs it.
    pub fn area_start(&self) -> usize {
        let end = self.ring_start() + self.ring_buffers as usize * self.ring_buffer_size as usize;
        if self.area_pages == 0 {
            end
        } else {
            end.next_multiple_of(Self::PAGE as usize)
        }
    }

    fn content_bytes(&self) -> usize {
        self.area_start() + self.area_pages as usize * Self::PAGE as usize
    }

    /// Bytes to allocate for the one pool holding every buffer, including
    /// one `block_size` of head-alignment padding when non-empty.
    pub fn pool_bytes(&self) -> usize {
        self.validate();
        if self.buffer_count() == 0 {
            0
        } else {
            self.content_bytes() + self.block_size as usize
        }
    }

    /// Offset into the raw pool allocation where the logical block-aligned
    /// pool view begins.
    pub fn pool_base(&self, raw_ptr: usize) -> usize {
        if self.buffer_count() == 0 || self.block_size == 0 {
            0
        } else {
            let align = self.block_size as usize;
            (align - (raw_ptr % align)) % align
        }
    }

    /// Where buffer `id` lies in the logical pool: its offset and its length.
    pub fn span(&self, id: BufId) -> (usize, usize) {
        self.validate();
        assert!(id.0 < self.buffer_count(), "buffer out of range");
        if id.0 < self.buffers {
            (
                id.0 as usize * self.buffer_size as usize,
                self.buffer_size as usize,
            )
        } else if id.0 < self.buffers + self.small_buffers {
            let large = self.buffers as usize * self.buffer_size as usize;
            (
                large + (id.0 - self.buffers) as usize * self.small_buffer_size as usize,
                self.small_buffer_size as usize,
            )
        } else if id.0 < self.ring_base() {
            let idx = (id.0 - self.buffers - self.small_buffers) as usize;
            (
                self.aligned_start() + idx * self.aligned_buffer_size as usize,
                self.aligned_buffer_size as usize,
            )
        } else if id.0 < self.area_base() {
            let idx = (id.0 - self.ring_base()) as usize;
            (
                self.ring_start() + idx * self.ring_buffer_size as usize,
                self.ring_buffer_size as usize,
            )
        } else {
            let idx = (id.0 - self.area_base()) as usize;
            (
                self.area_start() + idx * Self::PAGE as usize,
                Self::PAGE as usize,
            )
        }
    }

    /// Like [`Limits::span`] for the range `off..off + len` of the buffer.
    pub fn range(&self, id: BufId, off: u32, len: u32) -> core::ops::Range<usize> {
        let (base, size) = self.span(id);
        assert!(
            u64::from(off) + u64::from(len) <= size as u64,
            "buffer range out of bounds"
        );
        base + off as usize..base + off as usize + len as usize
    }

    /// Two distinct buffers of the pool, borrowed at once.
    pub fn pair<'a>(&self, pool: &'a mut [u8], a: BufId, b: BufId) -> (&'a mut [u8], &'a mut [u8]) {
        assert!(a != b, "buf_pair needs two distinct pool buffers");
        let ((a_at, a_len), (b_at, b_len)) = (self.span(a), self.span(b));
        if a_at < b_at {
            let (left, right) = pool.split_at_mut(b_at);
            (&mut left[a_at..a_at + a_len], &mut right[..b_len])
        } else {
            let (left, right) = pool.split_at_mut(a_at);
            (&mut right[..a_len], &mut left[b_at..b_at + b_len])
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::BASE
    }
}
