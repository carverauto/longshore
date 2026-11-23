// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Sizes and token kinds shared by the server's parts.

/// Room for an object's request target.
pub(super) const TARGET_MAX: usize = 256;
/// A listing's request target: a prefix of up to `TARGET_MAX` bytes with
/// every byte percent-encoded, and a continuation token for a cursor of up
/// to `TARGET_MAX` bytes.
pub(super) const LIST_TARGET_MAX: usize =
    4 * TARGET_MAX + b"continuation-token=t&".len() + 2 * TARGET_MAX;
/// Room for the headers that describe an object: content type and metadata.
pub(super) const DESCRIBING_MAX: usize = 192;

/// Token kinds, added to a connection's token: a receive, a send, a close.
pub(super) const K_RECV: u64 = 0;
pub(super) const K_SEND: u64 = 1;
pub(super) const K_CLOSE: u64 = 2;

/// Behaviors [`super::Server::script`] can queue at once.
pub(super) const SCRIPT_MAX: usize = 32;
