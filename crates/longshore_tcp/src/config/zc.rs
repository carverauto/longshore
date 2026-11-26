// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a set that receives in pages (`Rx::ZeroCopy`) asks of its
//! backend, and what it must have itself.
//!
//! The plan checks what it can know: the receive area holds the pages
//! frames may keep, a page a connection for a header kept while its
//! connection waits for frame memory, a page for every piece a turn can
//! bring, and the spare pages the operator gives for what the kernel
//! holds. It cannot know what the card's ring and the sockets' receive
//! queues hold; `Sizes::with_zc_spare` is for those.

use super::{ConfigError, Sizes};
use longshore_common::io::Limits;

/// The most pieces a backend can have out at once: its ring of returns
/// has no more entries.
const PIECES_MOST: u32 = 32_768;

/// Buffers of frame memory's size that hold the tail blocks, one block a
/// frame in pages.
pub(super) const fn tail_bufs(s: &Sizes) -> u32 {
    if s.zc_pages == 0 {
        return 0;
    }
    let blocks = s.frame_size / Limits::PAGE;
    s.zc_frames.div_ceil(if blocks == 0 { 1 } else { blocks })
}

/// A set with pages has runs and piece records for them, a frame buffer
/// for every connection, and no more pieces than a backend can have out.
pub(super) fn check(s: &Sizes, conns: u32) -> Result<(), ConfigError> {
    if s.zc_pages == 0 {
        return Ok(());
    }
    if s.frames < conns {
        return Err(ConfigError::Frames {
            have: s.frames,
            need: conns,
        });
    }
    let out = s.zc_pieces.checked_add(conns);
    if s.zc_frames == 0 || s.zc_pieces == 0 || out.is_none_or(|n| n > PIECES_MOST) {
        return Err(ConfigError::Pages);
    }
    Ok(())
}

/// `limits` with the receive area a set of `conns` connections asks for,
/// and an operation a connection more: the cancel of a receive that a
/// full completion ring left silent.
pub(super) fn area(limits: Limits, s: &Sizes, conns: u32) -> Limits {
    if s.zc_pages == 0 {
        return limits;
    }
    let ops = limits.ops_max().saturating_add(conns);
    // What one turn can bring: a ring reports as many completions as it
    // has entries, two for each operation and two more.
    let a_turn = ops.saturating_mul(2).saturating_add(2);
    let pages = s
        .zc_pages
        .saturating_add(conns)
        .saturating_add(a_turn)
        .saturating_add(s.zc_spare);
    limits
        .with_ops_max(ops)
        .with_area_pages(limits.area_pages().saturating_add(pages))
}
