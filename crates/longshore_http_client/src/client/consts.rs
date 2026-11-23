// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The client's constants: limits, and the operation kinds in its tokens.

/// The default and largest `tls_records_max`.
pub const TLS_RECORDS_MAX: u64 = longshore_stream::KEY_RECORDS_RETIRE;

/// Largest body the client will read and discard for a caller with no buffer.
pub(super) const DRAIN_MAX: u64 = 256 * 1024;

// Each connection owns eight tokens; these are the kinds in use.
pub(super) const K_CONNECT: u64 = 0;
pub(super) const K_SEND: u64 = 1;
pub(super) const K_RECV: u64 = 2;
pub(super) const K_CLOSE: u64 = 3;
pub(super) const K_CANCEL: u64 = 4;
