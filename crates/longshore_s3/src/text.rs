// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Text this crate wrote into a byte buffer, read back as `&str`.
//!
//! Requests are built without allocating, in fixed byte buffers: digits,
//! timestamps, base64, percent-encodings, and whole `&str` pieces such as
//! keys that were checked when they were given. What comes out is text by
//! construction, and [`written`] is the one place that says so.

/// `bytes` as text. Only for bytes this crate wrote from ASCII or from
/// whole `&str` pieces; never for bytes read from a peer.
#[expect(
    clippy::expect_used,
    reason = "every caller passes bytes it wrote from ASCII or whole &str pieces, so they are UTF-8"
)]
pub(crate) fn written(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).expect("bytes written as text")
}
