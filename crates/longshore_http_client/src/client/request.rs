// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A request as the caller describes it. It is built and read for every
//! request on the data path, so building and reading it copy nothing and
//! allocate nothing.

use longshore_common::io::BufId;
use std::net::SocketAddr;

/// A request for the client to send.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct Request<'a> {
    method: &'a str,
    target: &'a str,
    host: &'a str,
    host_id: u16,
    addr: SocketAddr,
    headers: &'a [(&'a str, &'a [u8])],
    body: Option<(BufId, u32)>,
    response_buf: Option<BufId>,
}

impl<'a> Request<'a> {
    /// A request with no extra headers, no body, and no buffer for a
    /// response body; the `with_` setters add those.
    #[inline]
    pub const fn new(
        method: &'a str,
        target: &'a str,
        host: &'a str,
        host_id: u16,
        addr: SocketAddr,
    ) -> Request<'a> {
        Request {
            method,
            target,
            host,
            host_id,
            addr,
            headers: &[],
            body: None,
            response_buf: None,
        }
    }

    /// Headers sent after Host (and Content-Length, when there is one).
    #[inline]
    pub const fn with_headers(mut self, headers: &'a [(&'a str, &'a [u8])]) -> Self {
        self.headers = headers;
        self
    }

    /// The request body: a pool buffer and the number of bytes to send
    /// from it, or `None` for no body.
    #[inline]
    pub const fn with_body(mut self, body: Option<(BufId, u32)>) -> Self {
        self.body = body;
        self
    }

    /// Where to receive the response body, or `None` to read and discard
    /// it. Required if the response has a body larger than can be discarded.
    #[inline]
    pub const fn with_response_buf(mut self, response_buf: Option<BufId>) -> Self {
        self.response_buf = response_buf;
        self
    }

    /// The method: `GET`, `PUT`, `HEAD` and so on.
    #[inline]
    #[must_use]
    pub const fn method(&self) -> &'a str {
        self.method
    }

    /// Origin-form target, already percent-encoded: `/bucket/key?query`.
    #[inline]
    #[must_use]
    pub const fn target(&self) -> &'a str {
        self.target
    }

    /// Value of the Host header.
    #[inline]
    #[must_use]
    pub const fn host(&self) -> &'a str {
        self.host
    }

    /// Connections are reused only for the same `host_id`.
    #[inline]
    #[must_use]
    pub const fn host_id(&self) -> u16 {
        self.host_id
    }

    /// Address to connect to if a new connection is needed.
    #[inline]
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Headers sent after Host (and Content-Length, when there is one).
    #[inline]
    #[must_use]
    pub const fn headers(&self) -> &'a [(&'a str, &'a [u8])] {
        self.headers
    }

    /// Request body: a pool buffer and the number of bytes to send from it.
    #[inline]
    #[must_use]
    pub const fn body(&self) -> Option<(BufId, u32)> {
        self.body
    }

    /// Where to receive the response body.
    #[inline]
    #[must_use]
    pub const fn response_buf(&self) -> Option<BufId> {
        self.response_buf
    }
}
