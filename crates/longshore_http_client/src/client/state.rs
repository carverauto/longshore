// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where a connection is in its exchange.

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum State {
    Closed,
    /// Closing an old connection; a loaded request connects afterwards.
    Closing {
        then_connect: bool,
    },
    Connecting,
    /// An idle connection is being looked at before it is used again; the
    /// request's head is already in the head buffer.
    CheckIdle,
    /// TCP is up; the TLS handshake is in progress.
    #[cfg(feature = "tls")]
    TlsHandshake,
    SendHead {
        off: u32,
    },
    SendBody {
        off: u32,
    },
    RecvHead {
        have: u32,
    },
    RecvBody {
        have: u32,
    },
    /// Reading and discarding a body the caller gave no buffer for (an
    /// error document, typically), through the spare room in the head buffer.
    Drain {
        have: u32,
    },
    /// Receiving a chunked body; `decoded` is the offset in the body's
    /// buffer where decoded bytes end and newly received bytes go.
    RecvChunked {
        decoded: u32,
    },
    /// The chunked body filled the caller's buffer exactly; only the end of
    /// the framing is still to come, read into the space after the head.
    RecvChunkedTail,
    Idle,
}
