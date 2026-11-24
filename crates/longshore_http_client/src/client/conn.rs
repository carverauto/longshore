// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One connection of the pool and the request it carries.

use super::State;
use crate::chunked::Chunked;
use crate::head::Fields;
use longshore_common::io::BufId;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Clone, Copy)]
pub(super) struct Conn {
    pub(super) state: State,
    pub(super) host_id: u16,
    pub(super) addr: SocketAddr,
    pub(super) opened_at: u64,
    pub(super) uses: u32,
    /// Maximum segment size measured when the connection opened, or 0.
    pub(super) segment: u32,
    /// Not to be reused: replaced the next time it would be picked.
    pub(super) retired: bool,
    // Current request.
    pub(super) token: u64,
    pub(super) started_at: u64,
    pub(super) head_len: u32,
    pub(super) body: Option<(BufId, u32)>,
    pub(super) response_buf: Option<BufId>,
    pub(super) is_head: bool,
    pub(super) timed_out: bool,
    /// The head of the current request was handed to the `Io` for sending.
    pub(super) sent: bool,
    pub(super) status: u16,
    pub(super) body_len: u32,
    pub(super) fields: Option<Fields>,
    /// Chunked decoding state, and where the body starts in its buffer.
    pub(super) chunked: Chunked,
    pub(super) body_base: u32,
    /// Server name for TLS, copied from the request's host.
    #[cfg(feature = "tls-core")]
    pub(super) name: [u8; 253],
    #[cfg(feature = "tls-core")]
    pub(super) name_len: u8,
}

impl Conn {
    /// A closed connection that has never been used.
    pub(super) fn new() -> Conn {
        Conn {
            state: State::Closed,
            host_id: 0,
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            opened_at: 0,
            uses: 0,
            segment: 0,
            retired: false,
            token: 0,
            started_at: 0,
            head_len: 0,
            body: None,
            response_buf: None,
            is_head: false,
            timed_out: false,
            sent: false,
            status: 0,
            body_len: 0,
            fields: None,
            chunked: Chunked::new(),
            body_base: 0,
            #[cfg(feature = "tls-core")]
            name: [0; 253],
            #[cfg(feature = "tls-core")]
            name_len: 0,
        }
    }
}
