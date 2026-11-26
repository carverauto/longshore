// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters and `with_*` setters for each of the [`Limits`].

use super::Limits;

impl Limits {
    /// Socket slots (`Sock(0..sockets_max)`).
    #[inline]
    pub const fn sockets_max(self) -> u32 {
        self.sockets_max
    }

    /// This value with [`Limits::sockets_max`] set to `sockets_max`.
    #[inline]
    pub const fn with_sockets_max(mut self, sockets_max: u32) -> Self {
        self.sockets_max = sockets_max;
        self
    }

    /// First socket slot in the range reserved for [`crate::io::Op::ArmedAccept`].
    #[inline]
    pub const fn accept_range_start(self) -> u32 {
        self.accept_range_start
    }

    /// This value with [`Limits::accept_range_start`] set to `accept_range_start`.
    #[inline]
    pub const fn with_accept_range_start(mut self, accept_range_start: u32) -> Self {
        self.accept_range_start = accept_range_start;
        self
    }

    /// Number of socket slots reserved for [`crate::io::Op::ArmedAccept`]. Zero
    /// leaves the accept range empty.
    #[inline]
    pub const fn accept_range_len(self) -> u32 {
        self.accept_range_len
    }

    /// This value with [`Limits::accept_range_len`] set to `accept_range_len`.
    #[inline]
    pub const fn with_accept_range_len(mut self, accept_range_len: u32) -> Self {
        self.accept_range_len = accept_range_len;
        self
    }

    /// Large buffers, sized for object bodies: ids `0..buffers`.
    #[inline]
    pub const fn buffers(self) -> u32 {
        self.buffers
    }

    /// This value with [`Limits::buffers`] set to `buffers`.
    #[inline]
    pub const fn with_buffers(mut self, buffers: u32) -> Self {
        self.buffers = buffers;
        self
    }

    /// Bytes in each large buffer.
    #[inline]
    pub const fn buffer_size(self) -> u32 {
        self.buffer_size
    }

    /// This value with [`Limits::buffer_size`] set to `buffer_size`.
    #[inline]
    pub const fn with_buffer_size(mut self, buffer_size: u32) -> Self {
        self.buffer_size = buffer_size;
        self
    }

    /// Small buffers, sized for request and response heads and datagrams:
    /// ids `buffers..buffers + small_buffers`. May be zero.
    #[inline]
    pub const fn small_buffers(self) -> u32 {
        self.small_buffers
    }

    /// This value with [`Limits::small_buffers`] set to `small_buffers`.
    #[inline]
    pub const fn with_small_buffers(mut self, small_buffers: u32) -> Self {
        self.small_buffers = small_buffers;
        self
    }

    /// Bytes in each small buffer.
    #[inline]
    pub const fn small_buffer_size(self) -> u32 {
        self.small_buffer_size
    }

    /// This value with [`Limits::small_buffer_size`] set to `small_buffer_size`.
    #[inline]
    pub const fn with_small_buffer_size(mut self, small_buffer_size: u32) -> Self {
        self.small_buffer_size = small_buffer_size;
        self
    }

    /// Fixed file slots (`FileSlot(0..files_max)`). May be zero.
    #[inline]
    pub const fn files_max(self) -> u32 {
        self.files_max
    }

    /// This value with [`Limits::files_max`] set to `files_max`.
    #[inline]
    pub const fn with_files_max(mut self, files_max: u32) -> Self {
        self.files_max = files_max;
        self
    }

    /// Block-aligned buffers for direct file I/O: ids
    /// `buffers + small_buffers..buffers + small_buffers + aligned_buffers`.
    /// May be zero.
    #[inline]
    pub const fn aligned_buffers(self) -> u32 {
        self.aligned_buffers
    }

    /// This value with [`Limits::aligned_buffers`] set to `aligned_buffers`.
    #[inline]
    pub const fn with_aligned_buffers(mut self, aligned_buffers: u32) -> Self {
        self.aligned_buffers = aligned_buffers;
        self
    }

    /// Bytes in each block-aligned buffer.
    #[inline]
    pub const fn aligned_buffer_size(self) -> u32 {
        self.aligned_buffer_size
    }

    /// This value with [`Limits::aligned_buffer_size`] set to `aligned_buffer_size`.
    #[inline]
    pub const fn with_aligned_buffer_size(mut self, aligned_buffer_size: u32) -> Self {
        self.aligned_buffer_size = aligned_buffer_size;
        self
    }

    /// Block alignment in bytes for aligned buffers and hole punching.
    /// Must be a power of two `>= 4096` when `aligned_buffers > 0`.
    #[inline]
    pub const fn block_size(self) -> u32 {
        self.block_size
    }

    /// This value with [`Limits::block_size`] set to `block_size`.
    #[inline]
    pub const fn with_block_size(mut self, block_size: u32) -> Self {
        self.block_size = block_size;
        self
    }

    /// Buffers in the registered buffer ring (`<= 32_768`): ids
    /// `ring_base()..ring_base() + ring_buffers`. May be zero.
    #[inline]
    pub const fn ring_buffers(self) -> u32 {
        self.ring_buffers
    }

    /// This value with [`Limits::ring_buffers`] set to `ring_buffers`.
    #[inline]
    pub const fn with_ring_buffers(mut self, ring_buffers: u32) -> Self {
        self.ring_buffers = ring_buffers;
        self
    }

    /// Pages of the receive area, which a zero-copy receive
    /// ([`crate::io::Op::ArmedRecvZc`]) delivers pieces of: buffers of
    /// [`Limits::PAGE`] bytes each, with ids from [`Limits::area_base`].
    /// May be zero.
    #[inline]
    pub const fn area_pages(self) -> u32 {
        self.area_pages
    }

    /// This value with [`Limits::area_pages`] set to `area_pages`.
    #[inline]
    pub const fn with_area_pages(mut self, area_pages: u32) -> Self {
        self.area_pages = area_pages;
        self
    }

    /// Size in bytes of each buffer-ring buffer. Must be `> 0` when
    /// `ring_buffers > 0`.
    #[inline]
    pub const fn ring_buffer_size(self) -> u32 {
        self.ring_buffer_size
    }

    /// This value with [`Limits::ring_buffer_size`] set to `ring_buffer_size`.
    #[inline]
    pub const fn with_ring_buffer_size(mut self, ring_buffer_size: u32) -> Self {
        self.ring_buffer_size = ring_buffer_size;
        self
    }

    /// Maximum operations in flight. Armed operations (`ArmedAccept`,
    /// `ArmedRecv`) stay in flight across completions and count toward this
    /// limit until their final completion.
    #[inline]
    pub const fn ops_max(self) -> u32 {
        self.ops_max
    }

    /// This value with [`Limits::ops_max`] set to `ops_max`.
    #[inline]
    pub const fn with_ops_max(mut self, ops_max: u32) -> Self {
        self.ops_max = ops_max;
        self
    }
}
