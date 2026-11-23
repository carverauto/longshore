// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where a connection slot is in its life.

/// Where a connection slot is in its life.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum State {
    Free,
    /// Reserved for the accept in flight.
    Accepting,
    /// Accepted; the TLS handshake is in progress.
    #[cfg(feature = "tls")]
    Handshake,
    RecvHead {
        have: u32,
    },
    RecvBody {
        have: u32,
    },
    SendHead {
        off: u32,
    },
    SendBody {
        off: u32,
    },
    Stalled,
    Closing,
}
