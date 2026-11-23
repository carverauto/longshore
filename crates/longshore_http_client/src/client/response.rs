// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A successful response. Only the client makes one.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    status: u16,
    body_len: u32,
    conn: u32,
}

impl Response {
    #[inline]
    pub(crate) const fn new(status: u16, body_len: u32, conn: u32) -> Response {
        Response {
            status,
            body_len,
            conn,
        }
    }

    /// The status code.
    #[inline]
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Length of the response body. If no response buffer was given the
    /// body was read and discarded.
    #[inline]
    #[must_use]
    pub const fn body_len(&self) -> u32 {
        self.body_len
    }

    /// Connection that served it; pass to [`Client::header`](super::Client::header).
    #[inline]
    #[must_use]
    pub const fn conn(&self) -> u32 {
        self.conn
    }
}
