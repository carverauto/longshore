// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a server is made with: its slots, buffers and tokens on the `Io`.

use super::server_config_builder::ServerConfigBuilder;
use longshore_common::io::Sock;

/// Maps a body to the checksum header value it must carry.
///
/// Two forms are understood. Twelve base64 characters are a CRC-64/NVME as S3
/// carries it in `x-amz-checksum-crc64nvme`. Eight base64 characters followed
/// by four zero bytes are a CRC-32C as Google Cloud Storage carries it in
/// `x-goog-hash: crc32c=...`.
pub type ChecksumFn = fn(&[u8]) -> [u8; 12];

/// A valid server configuration, made with [`ServerConfig::builder`].
#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    listener: Sock,
    connections_max: u32,
    first_sock: u32,
    first_buf: u32,
    head_max: u32,
    buffer_size: u32,
    objects_max: u32,
    token_base: u64,
    checksum: Option<ChecksumFn>,
}

impl ServerConfig {
    /// Start a configuration from what every server needs: the listening
    /// socket, how many connections it serves, where its socket slots,
    /// buffers and tokens start on the `Io`, and the size of its buffers.
    /// The rest have defaults; see [`ServerConfigBuilder`].
    pub fn builder(
        listener: Sock,
        connections_max: u32,
        first_sock: u32,
        first_buf: u32,
        buffer_size: u32,
        token_base: u64,
    ) -> ServerConfigBuilder {
        ServerConfigBuilder::new(
            listener,
            connections_max,
            first_sock,
            first_buf,
            buffer_size,
            token_base,
        )
    }

    /// The configuration `b` describes. Only [`ServerConfigBuilder::build`]
    /// calls this, once it has checked `b`.
    pub(super) fn from_checked(b: ServerConfigBuilder) -> ServerConfig {
        ServerConfig {
            listener: b.listener,
            connections_max: b.connections_max,
            first_sock: b.first_sock,
            first_buf: b.first_buf,
            head_max: b.head_max,
            buffer_size: b.buffer_size,
            objects_max: b.objects_max,
            token_base: b.token_base,
            checksum: b.checksum,
        }
    }

    /// The listening socket.
    pub fn listener(&self) -> Sock {
        self.listener
    }

    /// Connections served at once.
    pub fn connections_max(&self) -> u32 {
        self.connections_max
    }

    /// Socket slots are `first_sock..first_sock + connections_max`.
    pub fn first_sock(&self) -> u32 {
        self.first_sock
    }

    /// Two buffers per connection, head then body, start here.
    pub fn first_buf(&self) -> u32 {
        self.first_buf
    }

    /// The longest request head read, and room for a response head.
    pub fn head_max(&self) -> u32 {
        self.head_max
    }

    /// Size of every buffer, and the largest object or request body.
    pub fn buffer_size(&self) -> u32 {
        self.buffer_size
    }

    /// Objects the store can hold. Each reserves `buffer_size` bytes at init.
    pub fn objects_max(&self) -> u32 {
        self.objects_max
    }

    /// Tokens used are `token_base..token_base + connections_max * 8 + 1`.
    pub fn token_base(&self) -> u64 {
        self.token_base
    }

    /// If set, the value an `x-amz-checksum-crc64nvme` header must have for
    /// a given body. A PUT whose header disagrees is refused with a
    /// `BadDigest` error document, and GET returns the stored value.
    pub fn checksum(&self) -> Option<ChecksumFn> {
        self.checksum
    }
}
