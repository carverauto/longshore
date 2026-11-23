// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Building a server configuration, and the checks that make it valid.

use super::server_config::{ChecksumFn, ServerConfig};
use super::server_config_error::ServerConfigError;
use longshore_common::io::Sock;

/// Room for the longest response head the server writes: a status line,
/// its checksum, tag, date, identity and range headers, and the
/// describing headers an object was stored with.
const HEAD_MAX_MIN: u32 = 1024;
/// The longest request head the parser reads: `u16::MAX`.
const HEAD_MAX_MAX: u32 = 0xFFFF;

/// A server configuration being made; see [`ServerConfig::builder`].
///
/// Defaults: `head_max` 4096, `objects_max` 8, no `checksum`.
#[derive(Clone, Copy, Debug)]
pub struct ServerConfigBuilder {
    pub(super) listener: Sock,
    pub(super) connections_max: u32,
    pub(super) first_sock: u32,
    pub(super) first_buf: u32,
    pub(super) head_max: u32,
    pub(super) buffer_size: u32,
    pub(super) objects_max: u32,
    pub(super) token_base: u64,
    pub(super) checksum: Option<ChecksumFn>,
}

impl ServerConfigBuilder {
    pub(super) fn new(
        listener: Sock,
        connections_max: u32,
        first_sock: u32,
        first_buf: u32,
        buffer_size: u32,
        token_base: u64,
    ) -> ServerConfigBuilder {
        ServerConfigBuilder {
            listener,
            connections_max,
            first_sock,
            first_buf,
            head_max: 4096,
            buffer_size,
            objects_max: 8,
            token_base,
            checksum: None,
        }
    }

    /// The longest request head read, and room for a response head:
    /// `1024..=65535`, and no more than `buffer_size`.
    pub fn head_max(mut self, head_max: u32) -> Self {
        self.head_max = head_max;
        self
    }

    /// Objects the store can hold. Each reserves `buffer_size` bytes at
    /// init. With none, every store is answered 507.
    pub fn objects_max(mut self, objects_max: u32) -> Self {
        self.objects_max = objects_max;
        self
    }

    /// Check stored bodies against the checksum header `checksum` computes,
    /// and return it on reads. See [`ServerConfig::checksum`].
    pub fn checksum(mut self, checksum: ChecksumFn) -> Self {
        self.checksum = Some(checksum);
        self
    }

    /// The configuration, or why it would not work.
    pub fn build(self) -> Result<ServerConfig, ServerConfigError> {
        let (listener, conns) = (self.listener, self.connections_max);
        if conns == 0 {
            return Err(ServerConfigError::NoConnections);
        }
        if !(HEAD_MAX_MIN..=HEAD_MAX_MAX).contains(&self.head_max) {
            return Err(ServerConfigError::HeadMaxOutOfRange {
                head_max: self.head_max,
                min: HEAD_MAX_MIN,
                max: HEAD_MAX_MAX,
            });
        }
        if self.head_max > self.buffer_size {
            return Err(ServerConfigError::HeadMaxAboveBufferSize {
                head_max: self.head_max,
                buffer_size: self.buffer_size,
            });
        }
        let Some(socks_end) = self.first_sock.checked_add(conns) else {
            return Err(ServerConfigError::SocketsOverflow {
                first_sock: self.first_sock,
                connections_max: conns,
            });
        };
        if (self.first_sock..socks_end).contains(&listener.0) {
            return Err(ServerConfigError::ListenerInConnectionSockets {
                listener,
                first_sock: self.first_sock,
                connections_max: conns,
            });
        }
        if conns
            .checked_mul(2)
            .and_then(|n| self.first_buf.checked_add(n))
            .is_none()
        {
            return Err(ServerConfigError::BuffersOverflow {
                first_buf: self.first_buf,
                connections_max: conns,
            });
        }
        if (u64::from(conns) * 8 + 1)
            .checked_add(self.token_base)
            .is_none()
        {
            return Err(ServerConfigError::TokensOverflow {
                token_base: self.token_base,
                connections_max: conns,
            });
        }
        Ok(ServerConfig::from_checked(self))
    }
}
