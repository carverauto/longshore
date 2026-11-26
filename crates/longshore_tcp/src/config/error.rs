// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Why a set could not be planned.

/// Why [`super::Config::plan`] refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    /// No connections, or a count or size of zero where one is needed.
    Zero,
    /// The backend's `Limits` already reserve an accept range; armed
    /// accepts need it for this `Net`.
    AcceptRangeTaken,
    /// The backend already has block-aligned or small buffers of another
    /// size, or a buffer ring of another shape: a class has one size.
    ClassSize,
    /// `frame_size` is not a multiple of the backend's block size.
    FrameAlignment,
    /// A segment is too small for `header_max` (or, with TLS, smaller than
    /// `TLS_SEGMENT_MIN`), or a count overflows.
    TooSmall,
    /// With pages for `Rx::ZeroCopy`: fewer frame buffers than the set
    /// has connections. Each connection of the mode takes frame memory
    /// before it asks for a frame's body, so that nothing it receives
    /// waits for memory another arriving frame holds.
    Frames {
        /// Frame buffers the set was given.
        have: u32,
        /// Connections it can have: as many frame buffers are needed.
        need: u32,
    },
    /// With pages for `Rx::ZeroCopy`: no frames in pages or no piece
    /// records to go with them, or more pieces than a backend can have
    /// out at once (32,768).
    Pages,
}
