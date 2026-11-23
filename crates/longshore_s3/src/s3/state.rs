// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where an operation's slot is in its life.

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum State {
    Free,
    InFlight,
    /// Waiting to (re)try at this time.
    Waiting {
        until: u64,
    },
    /// Finished; the slot's `outcome` is waiting to be collected.
    Finished,
}
