// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Classifies the outcome of one attempt. Pure, so it is tested exhaustively.

use crate::http::client::{Error, Response};
use crate::tls::handshake::TlsError;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Class {
    Success,
    /// Try again now, on another connection.
    Retry,
    /// Try again after a backoff: the store asked us to slow down.
    RetryDelayed,
    /// Do not retry; report to the caller.
    Fatal,
}

/// What a conditional store's precondition failure means to the caller.
pub const STATUS_PRECONDITION_FAILED: u16 = 412;

pub fn classify(result: &Result<Response, Error>) -> Class {
    match result {
        Ok(r) => match r.status {
            200..=299 => Class::Success,
            // Throttling and overload: back off before retrying.
            429 | 503 => Class::RetryDelayed,
            // The request timed out at the server, or the server failed.
            408 | 500 | 502 | 504 => Class::Retry,
            // Everything else is a definite answer: bad request, forbidden,
            // not found, precondition failed, redirects we do not follow.
            _ => Class::Fatal,
        },
        Err(e) => match e {
            // The network or the peer failed; the request may be repeated.
            Error::Io(_) | Error::Closed { .. } | Error::Timeout => Class::Retry,
            // All connections busy: wait rather than spin.
            Error::Busy => Class::RetryDelayed,
            // A response we cannot parse will not improve on retry, and a
            // request we cannot build never will.
            Error::Head(_)
            | Error::BadRequest
            | Error::BodyTooLarge
            | Error::UnsupportedFraming => Class::Fatal,
            // An identity we cannot verify, or a configuration that cannot
            // work, is final. Any other TLS failure (a damaged record, a
            // handshake cut short) is a property of that connection.
            Error::Tls(
                TlsError::BadCertificate
                | TlsError::Setup
                | TlsError::UnsupportedSuite
                | TlsError::NotDone,
            ) => Class::Fatal,
            Error::Tls(_) => Class::Retry,
        },
    }
}

#[cfg(test)]
mod tests;
