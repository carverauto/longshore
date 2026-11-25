// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! A framing made from any numbers is refused or usable, and what it
//! reads from any header is what a plain reading of the same bytes gives:
//! the same length, or the same refusal. Never a panic.
//!
//! The first ten bytes describe the framing: the header's length and the
//! place of its length field (two bytes each, so up to 65,535), the
//! field's width, the byte order and what the field counts, and the
//! longest frame. The rest is a header to read.

use libfuzzer_sys::fuzz_target;
use longshore_tcp::{FrameError, HeaderFraming};

/// The frame length the field at `at` of `header` says, read the plain
/// way: byte by byte into a number, the header added unless counted.
fn plainly(
    header: &[u8],
    (header_len, at, width): (u32, u32, u8),
    (big, counts): (bool, bool),
    max_frame: u32,
) -> Result<usize, FrameError> {
    let field = &header[at as usize..at as usize + usize::from(width)];
    let mut said = 0u64;
    for i in 0..field.len() {
        let byte = if big { field[i] } else { field[field.len() - 1 - i] };
        said = said << 8 | u64::from(byte);
    }
    if counts && said < u64::from(header_len) {
        return Err(FrameError::TooSmall);
    }
    let whole = if counts {
        said
    } else {
        said.saturating_add(u64::from(header_len))
    };
    if whole > u64::from(max_frame) {
        return Err(FrameError::TooLarge);
    }
    Ok(whole as usize)
}

fuzz_target!(|data: &[u8]| {
    let Some((described, header)) = data.split_first_chunk::<10>() else {
        return;
    };
    let [h0, h1, a0, a1, width, flags, m0, m1, m2, m3] = *described;
    let header_len = u32::from(u16::from_le_bytes([h0, h1]));
    let len_at = u32::from(u16::from_le_bytes([a0, a1]));
    let (big, counts) = (flags & 1 != 0, flags & 2 != 0);
    let max_frame = u32::from_le_bytes([m0, m1, m2, m3]);
    let made = HeaderFraming::new(header_len, len_at, width, big, counts, max_frame);
    let Ok(framing) = made else {
        return;
    };
    assert_eq!(framing.header_len(), header_len as usize);
    assert_eq!(framing.max_frame(), max_frame as usize);
    assert!(framing.max_frame() >= framing.header_len());
    // It reads a header of its own length, whatever is in it.
    if header.len() < framing.header_len() {
        return;
    }
    let read = framing.frame_len(&header[..framing.header_len()]);
    let want = plainly(header, (header_len, len_at, width), (big, counts), max_frame);
    assert_eq!(read, want);
    // And the same for the same header with anything behind it.
    assert_eq!(framing.frame_len(header), read);
});
