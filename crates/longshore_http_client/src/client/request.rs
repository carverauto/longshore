// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A request as the caller describes it.

use longshore_common::io::BufId;
use std::net::SocketAddr;

pub struct Request<'a> {
    pub method: &'a str,
    /// Origin-form target, already percent-encoded: `/bucket/key?query`.
    pub target: &'a str,
    /// Value of the Host header.
    pub host: &'a str,
    /// Connections are reused only for the same `host_id`.
    pub host_id: u16,
    /// Address to connect to if a new connection is needed.
    pub addr: SocketAddr,
    pub headers: &'a [(&'a str, &'a [u8])],
    /// Request body: a pool buffer and the number of bytes to send from it.
    pub body: Option<(BufId, u32)>,
    /// Where to receive the response body. Required if the response has one.
    pub response_buf: Option<BufId>,
}
