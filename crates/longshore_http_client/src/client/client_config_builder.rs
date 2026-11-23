// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Building a [`ClientConfig`]: the values every client needs, defaults for
//! the rest, and a check of every limit before the configuration exists.

use super::{ClientConfig, ClientConfigError, TLS_RECORDS_MAX};

const MS: u64 = 1_000_000;

/// The default `head_max`.
pub const HEAD_MAX_DEFAULT: u32 = 8192;
/// The default `max_age_ns`: five minutes.
pub const MAX_AGE_NS_DEFAULT: u64 = 300_000 * MS;
/// The default `max_requests`.
pub const MAX_REQUESTS_DEFAULT: u32 = 100_000;
/// The default `request_timeout_ns`: one minute.
pub const REQUEST_TIMEOUT_NS_DEFAULT: u64 = 60_000 * MS;
/// The default `zero_copy_min`.
pub const ZERO_COPY_MIN_DEFAULT: u32 = 64 * 1024;
/// The largest default `tls_batch`; a smaller buffer size is used when it
/// is smaller.
pub const TLS_BATCH_DEFAULT_MAX: u32 = 3 << 20;

/// A [`ClientConfig`] being put together. Made by [`ClientConfig::builder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct ClientConfigBuilder {
    connections_max: u32,
    first_sock: u32,
    first_buf: u32,
    buffer_size: u32,
    token_base: u64,
    head_max: u32,
    max_age_ns: u64,
    max_requests: u32,
    request_timeout_ns: u64,
    zero_copy_min: u32,
    /// `None` until set: the buffer size, up to [`TLS_BATCH_DEFAULT_MAX`].
    tls_batch: Option<u32>,
    tls_records_max: u64,
    check_idle: bool,
}

impl ClientConfigBuilder {
    #[inline]
    pub(super) const fn new(
        connections_max: u32,
        first_sock: u32,
        first_buf: u32,
        buffer_size: u32,
        token_base: u64,
    ) -> ClientConfigBuilder {
        ClientConfigBuilder {
            connections_max,
            first_sock,
            first_buf,
            buffer_size,
            token_base,
            head_max: HEAD_MAX_DEFAULT,
            max_age_ns: MAX_AGE_NS_DEFAULT,
            max_requests: MAX_REQUESTS_DEFAULT,
            request_timeout_ns: REQUEST_TIMEOUT_NS_DEFAULT,
            zero_copy_min: ZERO_COPY_MIN_DEFAULT,
            tls_batch: None,
            tls_records_max: TLS_RECORDS_MAX,
            check_idle: true,
        }
    }

    /// Largest request or response head: at least 1, at most the buffer
    /// size and 65535. Default [`HEAD_MAX_DEFAULT`].
    #[inline]
    pub const fn head_max(mut self, head_max: u32) -> Self {
        self.head_max = head_max;
        self
    }

    /// An idle connection older than this is replaced before reuse; at
    /// least 1. Default [`MAX_AGE_NS_DEFAULT`].
    #[inline]
    pub const fn max_age_ns(mut self, max_age_ns: u64) -> Self {
        self.max_age_ns = max_age_ns;
        self
    }

    /// A connection is replaced after this many requests; at least 1.
    /// Default [`MAX_REQUESTS_DEFAULT`].
    #[inline]
    pub const fn max_requests(mut self, max_requests: u32) -> Self {
        self.max_requests = max_requests;
        self
    }

    /// A request taking longer than this is cancelled and fails; at least
    /// 1. Default [`REQUEST_TIMEOUT_NS_DEFAULT`].
    #[inline]
    pub const fn request_timeout_ns(mut self, request_timeout_ns: u64) -> Self {
        self.request_timeout_ns = request_timeout_ns;
        self
    }

    /// Request bodies at least this large are sent zero-copy. The kernel
    /// copies small zero-copy sends anyway, so this should be large.
    /// Default [`ZERO_COPY_MIN_DEFAULT`].
    #[inline]
    pub const fn zero_copy_min(mut self, zero_copy_min: u32) -> Self {
        self.zero_copy_min = zero_copy_min;
        self
    }

    /// With TLS, the most ciphertext sealed before each send. See
    /// `Stream::with_tls_buffers`. Default: the buffer size, up to
    /// [`TLS_BATCH_DEFAULT_MAX`].
    #[inline]
    pub const fn tls_batch(mut self, tls_batch: u32) -> Self {
        self.tls_batch = Some(tls_batch);
        self
    }

    /// With TLS, a connection that has protected this many records in
    /// either direction is closed after its current request, so traffic
    /// keys are replaced long before they wear out. At least 1, at most and
    /// by default [`TLS_RECORDS_MAX`].
    #[inline]
    pub const fn tls_records_max(mut self, tls_records_max: u64) -> Self {
        self.tls_records_max = tls_records_max;
        self
    }

    /// Before an idle connection carries another request, look at it
    /// without waiting. Bytes the last response did not declare, or a
    /// connection the peer has closed, mean the request goes out on a new
    /// connection. Costs one extra operation per reused connection.
    /// Default true.
    #[inline]
    pub const fn check_idle(mut self, check_idle: bool) -> Self {
        self.check_idle = check_idle;
        self
    }

    /// The configuration, or the first limit it breaks.
    pub fn build(self) -> Result<ClientConfig, ClientConfigError> {
        if self.connections_max == 0 || self.connections_max > u32::MAX / 3 {
            return Err(ClientConfigError::Connections);
        }
        if self.first_sock.checked_add(self.connections_max).is_none() {
            return Err(ClientConfigError::SocketRange);
        }
        if self.first_buf.checked_add(self.connections_max).is_none() {
            return Err(ClientConfigError::BufferRange);
        }
        if self
            .token_base
            .checked_add(8 * u64::from(self.connections_max))
            .is_none()
        {
            return Err(ClientConfigError::TokenRange);
        }
        if self.head_max == 0
            || self.head_max > self.buffer_size
            || self.head_max > u32::from(u16::MAX)
        {
            return Err(ClientConfigError::HeadMax);
        }
        if self.max_age_ns == 0 {
            return Err(ClientConfigError::MaxAge);
        }
        if self.max_requests == 0 {
            return Err(ClientConfigError::MaxRequests);
        }
        if self.request_timeout_ns == 0 {
            return Err(ClientConfigError::RequestTimeout);
        }
        if self.tls_records_max == 0 || self.tls_records_max > TLS_RECORDS_MAX {
            return Err(ClientConfigError::TlsRecordsMax);
        }
        let tls_batch = match self.tls_batch {
            Some(batch) => batch,
            None if self.buffer_size < TLS_BATCH_DEFAULT_MAX => self.buffer_size,
            None => TLS_BATCH_DEFAULT_MAX,
        };
        Ok(ClientConfig {
            connections_max: self.connections_max,
            first_sock: self.first_sock,
            first_buf: self.first_buf,
            head_max: self.head_max,
            buffer_size: self.buffer_size,
            max_age_ns: self.max_age_ns,
            max_requests: self.max_requests,
            request_timeout_ns: self.request_timeout_ns,
            zero_copy_min: self.zero_copy_min,
            token_base: self.token_base,
            tls_batch,
            tls_records_max: self.tls_records_max,
            check_idle: self.check_idle,
        })
    }
}
