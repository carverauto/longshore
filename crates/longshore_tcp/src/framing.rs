// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a byte stream is cut into frames.
//!
//! [`Framing::Bytes`] does not cut it: received bytes are passed on as they
//! come. [`Framing::Header`] delivers whole frames, each starting with a
//! header of fixed size that holds the frame's length at a fixed place: a
//! four-byte length prefix, or a larger header with the size somewhere
//! inside it.

/// Why a framing description is not usable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FramingError {
    /// The length field's width is not 1, 2, 4 or 8 bytes.
    Width,
    /// The length field does not lie inside the header.
    Field,
    /// `max_frame` is smaller than the header, or does not fit the length field.
    MaxFrame,
    /// The block a body is to start at is not a power of two.
    Block,
}

/// Why a header does not describe an acceptable frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameError {
    /// The frame would be longer than `max_frame`.
    TooLarge,
    /// The length counts the header and is smaller than it.
    TooSmall,
}

/// Frames with a fixed-size header that carries the frame's length.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HeaderFraming {
    header_len: u32,
    len_at: u32,
    len_width: u8,
    big_endian: bool,
    len_counts_header: bool,
    max_frame: u32,
    /// The block the body is to start at a boundary of; zero for none.
    body_block: u32,
}

impl HeaderFraming {
    /// A header of `header_len` bytes whose length field is `len_width`
    /// bytes at offset `len_at`, big- or little-endian. The field gives the
    /// length of the whole frame when `len_counts_header`, otherwise of
    /// what follows the header. A whole frame, header included, is at most
    /// `max_frame` bytes.
    pub const fn new(
        header_len: u32,
        len_at: u32,
        len_width: u8,
        big_endian: bool,
        len_counts_header: bool,
        max_frame: u32,
    ) -> Result<HeaderFraming, FramingError> {
        if !matches!(len_width, 1 | 2 | 4 | 8) {
            return Err(FramingError::Width);
        }
        if header_len == 0 || len_at as u64 + len_width as u64 > header_len as u64 {
            return Err(FramingError::Field);
        }
        // The largest value the field can hold, as a frame length.
        let field_max = if len_width >= 4 {
            u32::MAX as u64
        } else {
            (1u64 << (len_width as u32 * 8)) - 1
        };
        let most = if len_counts_header {
            field_max
        } else {
            field_max + header_len as u64
        };
        if max_frame < header_len || max_frame as u64 > most {
            return Err(FramingError::MaxFrame);
        }
        Ok(HeaderFraming {
            header_len,
            len_at,
            len_width,
            big_endian,
            len_counts_header,
            max_frame,
            body_block: 0,
        })
    }

    /// The same framing, with a frame received in direct mode laid so
    /// that its body, not its header, starts at a multiple of `block` in
    /// the frame's memory: the frame starts [`HeaderFraming::lead`] bytes
    /// into it. For a body that goes to a direct-I/O write from where it
    /// lies. `block` is a power of two; frame memory must hold the lead
    /// and the largest frame.
    pub const fn with_body_at_block(mut self, block: u32) -> Result<HeaderFraming, FramingError> {
        if !block.is_power_of_two() {
            return Err(FramingError::Block);
        }
        self.body_block = block;
        Ok(self)
    }

    /// Bytes in front of a frame in its memory, so that its body starts
    /// at a block boundary; zero unless
    /// [`HeaderFraming::with_body_at_block`] asked for it.
    #[inline]
    pub const fn lead(&self) -> u32 {
        if self.body_block == 0 {
            return 0;
        }
        (self.body_block - self.header_len % self.body_block) % self.body_block
    }

    /// A four-byte big-endian prefix giving the length of what follows it.
    pub const fn length_prefix(max_frame: u32) -> Result<HeaderFraming, FramingError> {
        HeaderFraming::new(4, 0, 4, true, false, max_frame)
    }

    /// Bytes in the header.
    #[inline]
    pub const fn header_len(&self) -> usize {
        self.header_len as usize
    }

    /// The longest frame accepted, header included.
    #[inline]
    pub const fn max_frame(&self) -> usize {
        self.max_frame as usize
    }

    /// The length of the whole frame, header included, that `header`
    /// starts. `header` must hold at least [`HeaderFraming::header_len`] bytes.
    #[inline]
    pub fn frame_len(&self, header: &[u8]) -> Result<usize, FrameError> {
        let field =
            &header[self.len_at as usize..(self.len_at + u32::from(self.len_width)) as usize];
        let mut value = 0u64;
        if self.big_endian {
            for b in field {
                value = (value << 8) | u64::from(*b);
            }
        } else {
            for b in field.iter().rev() {
                value = (value << 8) | u64::from(*b);
            }
        }
        let whole = if self.len_counts_header {
            if value < u64::from(self.header_len) {
                return Err(FrameError::TooSmall);
            }
            value
        } else {
            value.saturating_add(u64::from(self.header_len))
        };
        if whole > u64::from(self.max_frame) {
            return Err(FrameError::TooLarge);
        }
        Ok(whole as usize)
    }
}

/// How a connection's byte stream is delivered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Framing {
    /// As raw bytes, in whatever pieces they arrive.
    Bytes,
    /// As whole frames, each announced by its header.
    Header(HeaderFraming),
}
