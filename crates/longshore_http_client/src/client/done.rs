// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a request ended. Only the client makes one.

use super::{Error, Response};

/// A request that ended, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done {
    token: u64,
    result: Result<Response, Error>,
    sent: bool,
}

impl Done {
    #[inline]
    pub(crate) const fn new(token: u64, result: Result<Response, Error>, sent: bool) -> Done {
        Done {
            token,
            result,
            sent,
        }
    }

    /// The token the request was started with.
    #[inline]
    #[must_use]
    pub const fn token(&self) -> u64 {
        self.token
    }

    /// The response, or why there is none.
    #[inline]
    pub const fn result(&self) -> Result<Response, Error> {
        self.result
    }

    /// The request head was handed to the `Io` for sending. False means no
    /// byte of the request left this client, so the server cannot have
    /// acted on it. True does not prove the server received it.
    #[inline]
    #[must_use]
    pub const fn sent(&self) -> bool {
        self.sent
    }
}
