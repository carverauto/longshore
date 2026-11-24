// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Which library a client's HTTPS connections use, chosen with
//! [`Builder::tls_provider`](super::Builder::tls_provider).

/// A TLS provider. Each is a Cargo feature of this crate and of `longshore`;
/// several can be compiled in, and every variant exists in every build so
/// that code naming one compiles everywhere. Choosing one that is not
/// compiled in makes [`Builder::build`](super::Builder::build) fail with an
/// error that names its feature.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum TlsProvider {
    /// AWS-LC seals and opens records with no copy; the default. Feature
    /// `aws-lc` (or `tls`).
    AwsLc,
    /// ring seals and opens records, copying each body once. Feature `ring`.
    Ring,
    /// rustls does all of TLS, its record encryption included, with its
    /// ring provider. It allocates a buffer for every record it seals, inside
    /// the allocation exception the handshake uses. Feature `rustls`.
    Rustls,
}

impl TlsProvider {
    /// Whether this provider's feature is on in this build.
    #[must_use]
    pub const fn compiled(self) -> bool {
        match self {
            TlsProvider::AwsLc => cfg!(feature = "aws-lc"),
            TlsProvider::Ring => cfg!(feature = "ring"),
            TlsProvider::Rustls => cfg!(feature = "rustls"),
        }
    }

    /// What `build` uses when no provider is chosen: the first compiled in
    /// of AWS-LC, ring and rustls. `None` in a build without TLS.
    #[must_use]
    pub const fn preferred() -> Option<TlsProvider> {
        if cfg!(feature = "aws-lc") {
            Some(TlsProvider::AwsLc)
        } else if cfg!(feature = "ring") {
            Some(TlsProvider::Ring)
        } else if cfg!(feature = "rustls") {
            Some(TlsProvider::Rustls)
        } else {
            None
        }
    }

    /// The error `build` returns when this provider is chosen but not
    /// compiled in.
    pub(super) const fn missing(self) -> &'static str {
        match self {
            TlsProvider::AwsLc => {
                "TLS provider `aws-lc` is not compiled in: enable the `aws-lc` feature"
            }
            TlsProvider::Ring => {
                "TLS provider `ring` is not compiled in: enable the `ring` feature"
            }
            TlsProvider::Rustls => {
                "TLS provider `rustls` is not compiled in: enable the `rustls` feature"
            }
        }
    }
}

#[cfg(feature = "tls-core")]
impl From<TlsProvider> for longshore_tls::provider::Provider {
    fn from(p: TlsProvider) -> Self {
        match p {
            TlsProvider::AwsLc => longshore_tls::provider::Provider::AwsLc,
            TlsProvider::Ring => longshore_tls::provider::Provider::Ring,
            TlsProvider::Rustls => longshore_tls::provider::Provider::Rustls,
        }
    }
}
