// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Why a client configuration was refused.

use core::fmt;

/// Why a client configuration was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientConfigError {
    /// `connections_max` is zero, or so large that the operations the
    /// client can have in flight (three per connection) do not fit a `u32`.
    Connections,
    /// The socket slots `first_sock..first_sock + connections_max` run past `u32::MAX`.
    SocketRange,
    /// The head buffers `first_buf..first_buf + connections_max` run past `u32::MAX`.
    BufferRange,
    /// The tokens `token_base..token_base + connections_max * 8` run past `u64::MAX`.
    TokenRange,
    /// `head_max` is zero, larger than `buffer_size`, or larger than 65535.
    HeadMax,
    /// `max_age_ns` is zero: no connection could ever be reused.
    MaxAge,
    /// `max_requests` is zero.
    MaxRequests,
    /// `request_timeout_ns` is zero: every request would time out.
    RequestTimeout,
    /// `tls_records_max` is zero (every connection, plain ones too, would
    /// close after each request) or above `TLS_RECORDS_MAX`.
    TlsRecordsMax,
}

impl fmt::Display for ClientConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ClientConfigError::Connections => {
                "connections_max must be at least 1 and at most u32::MAX / 3"
            }
            ClientConfigError::SocketRange => {
                "first_sock + connections_max must not exceed u32::MAX"
            }
            ClientConfigError::BufferRange => {
                "first_buf + connections_max must not exceed u32::MAX"
            }
            ClientConfigError::TokenRange => {
                "token_base + 8 * connections_max must not exceed u64::MAX"
            }
            ClientConfigError::HeadMax => {
                "head_max must be at least 1 and at most buffer_size and 65535"
            }
            ClientConfigError::MaxAge => "max_age_ns must be at least 1",
            ClientConfigError::MaxRequests => "max_requests must be at least 1",
            ClientConfigError::RequestTimeout => "request_timeout_ns must be at least 1",
            ClientConfigError::TlsRecordsMax => {
                "tls_records_max must be at least 1 and at most TLS_RECORDS_MAX"
            }
        })
    }
}

impl std::error::Error for ClientConfigError {}
