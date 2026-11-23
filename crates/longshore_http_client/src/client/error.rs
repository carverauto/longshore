// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Why a request failed.

use crate::head::HttpError;
use longshore_stream::TlsError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Every connection is in use.
    Busy,
    /// The request head is invalid or does not fit.
    BadRequest,
    /// The response head is malformed or too large.
    Head(HttpError),
    /// A socket operation failed with this errno.
    Io(i32),
    /// The peer closed mid-exchange. `reused` is true when the connection had
    /// served earlier requests, the usual sign of a stale keep-alive.
    Closed {
        reused: bool,
    },
    Timeout,
    /// The response body does not fit the buffer, or it is too large to
    /// discard when no buffer was given.
    BodyTooLarge,
    /// A response body with neither a length nor chunked encoding (it would
    /// end only when the connection closes) is not supported.
    UnsupportedFraming,
    /// The TLS handshake failed or a record did not authenticate.
    Tls(TlsError),
}
