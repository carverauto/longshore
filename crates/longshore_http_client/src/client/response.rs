// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A successful response.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Length of the response body. If no response buffer was given the
    /// body was read and discarded.
    pub body_len: u32,
    /// Connection that served it; pass to [`Client::header`](super::Client::header).
    pub conn: u32,
}
