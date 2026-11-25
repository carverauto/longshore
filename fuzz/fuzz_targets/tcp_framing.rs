// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! A framing made from any numbers is refused or usable, and what it
//! reads from any header is refused or a frame length it allows: never a
//! panic, never a length below its header or above its longest frame.

use libfuzzer_sys::fuzz_target;
use longshore_tcp::HeaderFraming;

fuzz_target!(|data: &[u8]| {
    let Some((described, header)) = data.split_at_checked(12) else {
        return;
    };
    let number = |at: usize| u32::from_le_bytes([described[at], described[at + 1], 0, 0]);
    // A header of up to 65,535 bytes, a field anywhere, any width and
    // order, and any longest frame.
    let (header_len, len_at) = (number(0), number(2));
    let (width, flags) = (described[4], described[5]);
    let max_frame = u32::from_le_bytes([described[6], described[7], described[8], described[9]]);
    let made = HeaderFraming::new(
        header_len,
        len_at,
        width,
        flags & 1 != 0,
        flags & 2 != 0,
        max_frame,
    );
    let Ok(framing) = made else {
        return;
    };
    assert_eq!(framing.header_len(), header_len as usize);
    assert!(framing.max_frame() >= framing.header_len());
    // It reads a header of its own length, whatever is in it.
    if header.len() < framing.header_len() {
        return;
    }
    if let Ok(frame) = framing.frame_len(&header[..framing.header_len()]) {
        assert!(frame >= framing.header_len(), "{frame} is less than the header");
        assert!(frame <= framing.max_frame(), "{frame} is more than the longest");
    }
    // And the same answer for the same header with anything behind it.
    assert_eq!(
        framing.frame_len(header),
        framing.frame_len(&header[..framing.header_len()])
    );
});
