// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a request ended.

use super::{Error, Response};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done {
    pub token: u64,
    pub result: Result<Response, Error>,
    /// The request head was handed to the `Io` for sending. False means no
    /// byte of the request left this client, so the server cannot have
    /// acted on it. True does not prove the server received it.
    pub sent: bool,
}
