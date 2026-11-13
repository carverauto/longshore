// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::http::head::HttpError;

fn status(s: u16) -> Class {
    classify(&Ok(Response {
        status: s,
        body_len: 0,
        conn: 0,
    }))
}

#[test]
fn every_status_has_a_class() {
    for s in 100..600u16 {
        let expected = match s {
            200..=299 => Class::Success,
            429 | 503 => Class::RetryDelayed,
            408 | 500 | 502 | 504 => Class::Retry,
            _ => Class::Fatal,
        };
        assert_eq!(status(s), expected, "status {s}");
    }
    // The ones that matter most, spelled out.
    assert_eq!(status(200), Class::Success);
    assert_eq!(status(206), Class::Success, "ranged fetch");
    assert_eq!(status(204), Class::Success, "erase");
    assert_eq!(status(503), Class::RetryDelayed);
    assert_eq!(
        status(403),
        Class::Fatal,
        "bad credentials are never retried"
    );
    assert_eq!(status(404), Class::Fatal);
    assert_eq!(status(412), Class::Fatal, "conditional store lost");
    assert_eq!(status(301), Class::Fatal, "redirects are not followed");
}

#[test]
fn every_transport_error_has_a_class() {
    for (e, expected) in [
        (Error::Io(104), Class::Retry),
        (Error::Io(111), Class::Retry),
        (Error::Closed { reused: true }, Class::Retry),
        (Error::Closed { reused: false }, Class::Retry),
        (Error::Timeout, Class::Retry),
        (Error::Busy, Class::RetryDelayed),
        (Error::Head(HttpError::BadStartLine), Class::Fatal),
        (Error::BadRequest, Class::Fatal),
        (Error::BodyTooLarge, Class::Fatal),
        (Error::UnsupportedFraming, Class::Fatal),
        (Error::Tls(TlsError::BadCertificate), Class::Fatal),
        (Error::Tls(TlsError::Setup), Class::Fatal),
        (Error::Tls(TlsError::BadRecord), Class::Retry),
        (Error::Tls(TlsError::Handshake), Class::Retry),
    ] {
        assert_eq!(classify(&Err(e)), expected, "{e:?}");
    }
}
