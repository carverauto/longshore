// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A connection slot: its state and what the request on it asked for.

use super::consts::{DESCRIBING_MAX, LIST_TARGET_MAX, TARGET_MAX};
use super::state::State;
use super::{identity, multipart};

/// A connection slot: its state and what the request on it asked for.
#[derive(Clone, Copy)]
pub(super) struct Conn {
    pub(super) state: State,
    pub(super) target: [u8; LIST_TARGET_MAX],
    pub(super) target_len: usize,
    pub(super) method: [u8; 8],
    pub(super) method_len: usize,
    pub(super) request_len: u32,
    pub(super) head_len: u32,
    pub(super) body_len: u32,
    pub(super) close_after: bool,
    /// `If-None-Match: *` was sent: store only if the object does not exist.
    pub(super) if_none_match: bool,
    /// `Range: bytes=a-b`, inclusive.
    pub(super) range: Option<(u32, u32)>,
    /// Checksum header value sent with the request.
    pub(super) claimed: Option<[u8; 12]>,
    /// Checksum header value to send with the response.
    pub(super) reply_checksum: Option<[u8; 12]>,
    /// The request carried `x-amz-checksum-mode: ENABLED`.
    pub(super) checksum_mode: bool,
    /// The request's access key is on the allowed list (or no list is set).
    pub(super) authorized: bool,
    /// The request carried a bearer token: answer in Google Cloud Storage's dialect.
    pub(super) bearer: bool,
    /// `ETag` to send with the response: a stored part's tag.
    pub(super) reply_tag: Option<[u8; multipart::TAG_LEN]>,
    /// The request's content type and metadata headers, as raw lines.
    pub(super) describing: [u8; DESCRIBING_MAX],
    pub(super) describing_len: usize,
    /// The request's copy source, `/bucket/key`, if it is a copy.
    pub(super) copy_source: [u8; TARGET_MAX],
    pub(super) copy_source_len: usize,
    /// The object whose tag, date and describing headers go on the response.
    pub(super) reply_object: Option<u32>,
    /// What the request asked about the object's identity.
    pub(super) asked: identity::Asked,
}

impl Conn {
    /// A connection slot with nothing on it.
    pub(super) const FREE: Conn = Conn {
        state: State::Free,
        target: [0; LIST_TARGET_MAX],
        target_len: 0,
        method: [0; 8],
        method_len: 0,
        request_len: 0,
        head_len: 0,
        body_len: 0,
        close_after: false,
        if_none_match: false,
        range: None,
        claimed: None,
        reply_checksum: None,
        checksum_mode: false,
        authorized: true,
        bearer: false,
        reply_tag: None,
        describing: [0; DESCRIBING_MAX],
        describing_len: 0,
        copy_source: [0; TARGET_MAX],
        copy_source_len: 0,
        reply_object: None,
        asked: identity::Asked::NOTHING,
    };
}
