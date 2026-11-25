// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The client's configuration, valid by construction: it is made only by
//! [`ClientConfigBuilder::build`], which refuses a configuration the client
//! could not run with.

use super::ClientConfigBuilder;

/// How a client is laid out and how it behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    pub(super) connections_max: u32,
    pub(super) first_sock: u32,
    pub(super) first_buf: u32,
    pub(super) head_max: u32,
    pub(super) buffer_size: u32,
    pub(super) max_age_ns: u64,
    pub(super) max_requests: u32,
    pub(super) request_timeout_ns: u64,
    pub(super) zero_copy_min: u32,
    pub(super) token_base: u64,
    pub(super) tls_batch: u32,
    pub(super) tls_records_max: u64,
    pub(super) check_idle: bool,
}

impl ClientConfig {
    /// Start a configuration from the values every client needs: how many
    /// connections it keeps, where its socket slots, head buffers and
    /// tokens start, and the size of a pool buffer. Everything else has a
    /// default that a setter on the builder replaces.
    #[inline]
    pub const fn builder(
        connections_max: u32,
        first_sock: u32,
        first_buf: u32,
        buffer_size: u32,
        token_base: u64,
    ) -> ClientConfigBuilder {
        ClientConfigBuilder::new(
            connections_max,
            first_sock,
            first_buf,
            buffer_size,
            token_base,
        )
    }

    /// Connections in the pool, at least one.
    #[inline]
    #[must_use]
    pub const fn connections_max(&self) -> u32 {
        self.connections_max
    }

    /// Socket slots `first_sock..first_sock + connections_max`.
    #[inline]
    #[must_use]
    pub const fn first_sock(&self) -> u32 {
        self.first_sock
    }

    /// One head buffer per connection: `first_buf..first_buf + connections_max`.
    #[inline]
    #[must_use]
    pub const fn first_buf(&self) -> u32 {
        self.first_buf
    }

    /// Largest request or response head, at most the pool buffer size and 65535.
    #[inline]
    #[must_use]
    pub const fn head_max(&self) -> u32 {
        self.head_max
    }

    /// Size of a pool buffer; response bodies must fit in one.
    #[inline]
    #[must_use]
    pub const fn buffer_size(&self) -> u32 {
        self.buffer_size
    }

    /// An idle connection older than this is replaced before reuse.
    #[inline]
    #[must_use]
    pub const fn max_age_ns(&self) -> u64 {
        self.max_age_ns
    }

    /// A connection is replaced after this many requests.
    #[inline]
    #[must_use]
    pub const fn max_requests(&self) -> u32 {
        self.max_requests
    }

    /// A request taking longer than this is cancelled and fails.
    #[inline]
    #[must_use]
    pub const fn request_timeout_ns(&self) -> u64 {
        self.request_timeout_ns
    }

    /// Request bodies at least this large are sent zero-copy.
    #[inline]
    #[must_use]
    pub const fn zero_copy_min(&self) -> u32 {
        self.zero_copy_min
    }

    /// Tokens used are `token_base..token_base + connections_max * 8`.
    #[inline]
    #[must_use]
    pub const fn token_base(&self) -> u64 {
        self.token_base
    }

    /// With TLS, the most ciphertext sealed before each send. See
    /// `Stream::with_tls_buffers`.
    #[inline]
    #[must_use]
    pub const fn tls_batch(&self) -> u32 {
        self.tls_batch
    }

    /// With TLS, a connection that has protected this many records in either
    /// direction is closed after its current request.
    #[inline]
    #[must_use]
    pub const fn tls_records_max(&self) -> u64 {
        self.tls_records_max
    }

    /// Whether an idle connection is looked at before it carries another request.
    #[inline]
    #[must_use]
    pub const fn check_idle(&self) -> bool {
        self.check_idle
    }
}
