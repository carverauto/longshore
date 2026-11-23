// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where the credentials file reader is in its cycle.

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Provider {
    /// Waiting to reread at this time.
    Idle {
        at: u64,
    },
    Reading,
    /// A validation listing signed with the candidate is in progress.
    Validating,
}
