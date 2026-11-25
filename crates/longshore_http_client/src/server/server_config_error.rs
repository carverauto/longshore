// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Why a server configuration was refused.

use longshore_common::io::Sock;
use std::fmt;

/// Why [`super::ServerConfigBuilder::build`] refused a configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerConfigError {
    /// `connections_max` is zero: the server would never accept.
    NoConnections,
    /// `head_max` is below the room the server's own response heads need,
    /// or above the longest head the parser reads.
    HeadMaxOutOfRange {
        /// The `head_max` asked for.
        head_max: u32,
        /// The least it may be.
        min: u32,
        /// The most it may be.
        max: u32,
    },
    /// `head_max` is larger than the buffers the head is read into.
    HeadMaxAboveBufferSize {
        /// The `head_max` asked for.
        head_max: u32,
        /// The size of the buffers.
        buffer_size: u32,
    },
    /// `first_sock + connections_max` does not fit a socket slot number.
    SocketsOverflow {
        /// The first slot connections are accepted into.
        first_sock: u32,
        /// How many connections were asked for.
        connections_max: u32,
    },
    /// The listener is one of the slots connections are accepted into.
    ListenerInConnectionSockets {
        /// The listener's slot.
        listener: Sock,
        /// The first slot connections are accepted into.
        first_sock: u32,
        /// How many connections were asked for.
        connections_max: u32,
    },
    /// `first_buf + 2 * connections_max` does not fit a buffer id.
    BuffersOverflow {
        /// The server's first buffer.
        first_buf: u32,
        /// How many connections were asked for.
        connections_max: u32,
    },
    /// `token_base + connections_max * 8 + 1` does not fit a token.
    TokensOverflow {
        /// The server's first token.
        token_base: u64,
        /// How many connections were asked for.
        connections_max: u32,
    },
}

impl fmt::Display for ServerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ServerConfigError::NoConnections => {
                write!(f, "connections_max is zero: the server could not accept")
            }
            ServerConfigError::HeadMaxOutOfRange { head_max, min, max } => {
                write!(f, "head_max {head_max} is outside {min}..={max}")
            }
            ServerConfigError::HeadMaxAboveBufferSize {
                head_max,
                buffer_size,
            } => write!(f, "head_max {head_max} exceeds buffer_size {buffer_size}"),
            ServerConfigError::SocketsOverflow {
                first_sock,
                connections_max,
            } => write!(
                f,
                "socket slots {first_sock} + {connections_max} overflow a slot number"
            ),
            ServerConfigError::ListenerInConnectionSockets {
                listener,
                first_sock,
                connections_max,
            } => write!(
                f,
                "listener slot {} is inside the connection slots {first_sock}..{}",
                listener.get(),
                u64::from(first_sock) + u64::from(connections_max)
            ),
            ServerConfigError::BuffersOverflow {
                first_buf,
                connections_max,
            } => write!(
                f,
                "buffers {first_buf} + 2 * {connections_max} overflow a buffer id"
            ),
            ServerConfigError::TokensOverflow {
                token_base,
                connections_max,
            } => write!(
                f,
                "tokens {token_base} + 8 * {connections_max} + 1 overflow a token"
            ),
        }
    }
}

impl std::error::Error for ServerConfigError {}
