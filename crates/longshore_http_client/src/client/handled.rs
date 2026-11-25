// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the client made of a completion or a cancel.

use super::Done;

/// What the client made of a completion or a cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// The completion does not belong to this client.
    NotMine,
    /// Consumed; the request is still in progress.
    Pending,
    /// Consumed; a request ended.
    Done(Done),
}
