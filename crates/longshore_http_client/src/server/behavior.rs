// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How the server treats a request: normally, or scripted to misbehave.

/// How to treat the next request. Queued with [`super::Server::script`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behavior {
    /// Treat the request as the server ordinarily would.
    Normal,
    /// Respond with this status and an empty body.
    Status(u16),
    /// Respond with this status and a body of this many filler bytes, as
    /// real stores do with their error documents.
    StatusWithBody(u16, u32),
    /// Close the connection without responding.
    CloseBeforeResponse,
    /// Over TLS: answer with a fatal alert of this description, then close.
    TlsAlert(u8),
    /// Carry out the request, then close without responding: the response is lost.
    ApplyThenClose,
    /// Send the head and half the body, then close.
    TruncateBody,
    /// Read the request and never respond.
    Stall,
    /// Respond with bytes that are not HTTP.
    GarbageHead,
    /// Respond with a head that never ends.
    EndlessHead,
    /// Send exactly these bytes as the whole response, then close.
    Raw(&'static [u8]),
    /// Respond with this status and an error document naming this code.
    /// Real stores can do this with status 200.
    ErrorDocument(u16, &'static str),
    /// Respond with chunked transfer encoding and an empty body.
    Chunked,
    /// Serve a GET of a stored object with chunked transfer encoding, in
    /// chunks of this many bytes, on a connection that stays open.
    ChunkedBody(u32),
    /// Respond with chunked transfer encoding whose first chunk size is not a number.
    BadChunk,
}
