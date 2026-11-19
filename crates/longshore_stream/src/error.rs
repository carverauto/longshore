// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS error type and record-retirement threshold exposed regardless of the `tls` feature.

/// Past this many records in either direction a connection should carry no
/// further requests (half of the 2^24 hard limit).
pub const KEY_RECORDS_RETIRE: u64 = 1 << 23;

#[cfg(feature = "tls")]
const _: () = assert!(KEY_RECORDS_RETIRE == longshore_tls::record::KEY_RECORDS_RETIRE);

/// Failure from the TLS handshake or record layer.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TlsError {
    /// The peer's certificate chain or name did not validate.
    BadCertificate,
    /// The handshake failed for another reason: an alert, a protocol
    /// violation, no common parameters.
    Handshake,
    /// A received record is malformed.
    BadRecord,
    /// The certificate, key or server name given at setup is unusable.
    Setup,
    /// `finish` was called before the handshake completed.
    NotDone,
    /// A cipher suite other than AES-GCM was negotiated.
    UnsupportedSuite,
    /// The peer sent an alert other than an orderly close, after the
    /// handshake. The value is the alert's description code.
    Alert(u8),
    /// The record layer failed to seal or open a record.
    Record,
}

#[cfg(feature = "tls")]
impl From<longshore_tls::handshake::TlsError> for TlsError {
    fn from(e: longshore_tls::handshake::TlsError) -> Self {
        use longshore_tls::handshake::TlsError as Inner;
        match e {
            Inner::BadCertificate => TlsError::BadCertificate,
            Inner::Handshake => TlsError::Handshake,
            Inner::BadRecord => TlsError::BadRecord,
            Inner::Setup => TlsError::Setup,
            Inner::NotDone => TlsError::NotDone,
            Inner::UnsupportedSuite => TlsError::UnsupportedSuite,
            Inner::Alert(code) => TlsError::Alert(code),
            Inner::Record(_) => TlsError::Record,
        }
    }
}
