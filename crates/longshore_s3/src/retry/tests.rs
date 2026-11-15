// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;
use longshore_http_client::head::HttpError;

fn status(s: u16) -> Class {
    classify(&Ok(Response {
        status: s,
        body_len: 0,
        conn: 0,
    }))
}

/// Each status the policy treats specially, and its neighbours, written out
/// rather than computed: a change to the policy has to be made here too.
#[test]
fn statuses_are_classed_as_the_policy_says() {
    for (s, expected) in [
        (199, Class::Fatal),
        (200, Class::Success),
        (201, Class::Success),
        (299, Class::Success),
        (300, Class::Fatal),
        (304, Class::Fatal),
        (307, Class::Fatal),
        (400, Class::Fatal),
        (401, Class::Fatal),
        (407, Class::Fatal),
        (408, Class::Retry),
        (409, Class::Fatal),
        (416, Class::Fatal),
        (428, Class::Fatal),
        (429, Class::RetryDelayed),
        (430, Class::Fatal),
        (499, Class::Fatal),
        (500, Class::Retry),
        (501, Class::Fatal),
        (502, Class::Retry),
        (503, Class::RetryDelayed),
        (504, Class::Retry),
        (505, Class::Fatal),
        (507, Class::Fatal),
        (599, Class::Fatal),
        (100, Class::Fatal),
        (0, Class::Fatal),
        (999, Class::Fatal),
    ] {
        assert_eq!(status(s), expected, "status {s}");
    }
    // Exactly six statuses are retried, and nothing outside 2xx succeeds.
    let retried: Vec<u16> = (0..1000)
        .filter(|s| matches!(status(*s), Class::Retry | Class::RetryDelayed))
        .collect();
    assert_eq!(retried, [408, 429, 500, 502, 503, 504]);
    let succeeded = (0..1000).filter(|s| status(*s) == Class::Success);
    assert!(succeeded.eq(200..300));
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
        (Error::Tls(TlsError::UnsupportedSuite), Class::Fatal),
        (Error::Tls(TlsError::NotDone), Class::Fatal),
        // An alert after the handshake is a property of that connection.
        (Error::Tls(TlsError::Alert(20)), Class::Retry),
        (Error::Tls(TlsError::Alert(80)), Class::Retry),
        (
            Error::Tls(TlsError::Record(
                longshore_tls::record::RecordError::BadRecord,
            )),
            Class::Retry,
        ),
    ] {
        assert_eq!(classify(&Err(e)), expected, "{e:?}");
    }
}
