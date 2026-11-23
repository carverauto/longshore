// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The client's configuration.

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub connections_max: u32,
    /// Socket slots `first_sock..first_sock + connections_max`.
    pub first_sock: u32,
    /// One head buffer per connection: `first_buf..first_buf + connections_max`.
    pub first_buf: u32,
    /// Largest request or response head, at most the pool buffer size and 65535.
    pub head_max: u32,
    /// Size of a pool buffer; response bodies must fit in one.
    pub buffer_size: u32,
    /// An idle connection older than this is replaced before reuse.
    pub max_age_ns: u64,
    /// A connection is replaced after this many requests.
    pub max_requests: u32,
    /// A request taking longer than this is cancelled and fails.
    pub request_timeout_ns: u64,
    /// Request bodies at least this large are sent zero-copy. The kernel
    /// copies small zero-copy sends anyway, so this should be large.
    pub zero_copy_min: u32,
    /// Tokens used are `token_base..token_base + connections_max * 8`.
    pub token_base: u64,
    /// With TLS, the most ciphertext sealed before each send. See
    /// `Stream::with_tls_buffers`.
    pub tls_batch: u32,
    /// With TLS, a connection that has protected this many records in either
    /// direction is closed after its current request, so traffic keys are
    /// replaced long before they wear out. At most, and normally exactly,
    /// [`TLS_RECORDS_MAX`](super::TLS_RECORDS_MAX).
    pub tls_records_max: u64,
    /// Before an idle connection carries another request, look at it without
    /// waiting. Bytes the last response did not declare, or a connection the
    /// peer has closed, mean the request goes out on a new connection.
    /// Costs one extra operation per reused connection.
    pub check_idle: bool,
}
