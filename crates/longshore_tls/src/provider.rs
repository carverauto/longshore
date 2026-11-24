// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Which library seals and opens records and runs the handshake.
//!
//! Each provider is a Cargo feature; several can be compiled in at once,
//! since Cargo unifies features across a dependency graph, and the caller
//! picks one per [`ClientSetup`](crate::handshake::ClientSetup) or
//! [`ServerSetup`](crate::handshake::ServerSetup).

#[cfg(not(any(feature = "aws-lc", feature = "ring", feature = "rustls")))]
compile_error!("longshore_tls needs a provider feature: `aws-lc`, `ring` or `rustls`");

/// A TLS provider. Every variant exists in every build; [`Provider::compiled`]
/// says whether its feature is on.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum Provider {
    /// AWS-LC seals and opens records from the caller's buffer, with no
    /// copy; rustls's AWS-LC provider runs the handshake. Feature `aws-lc`.
    AwsLc,
    /// ring seals and opens records in place, copying each body once;
    /// rustls's ring provider runs the handshake. Feature `ring`.
    Ring,
    /// rustls seals and opens records itself, with its ring provider. It
    /// allocates a buffer for each record it seals; that runs inside the
    /// allocation exception the handshake uses. Feature `rustls`.
    Rustls,
}

impl Provider {
    /// Every provider, fastest first: the order [`Provider::preferred`]
    /// picks in.
    pub const ALL: [Provider; 3] = [Provider::AwsLc, Provider::Ring, Provider::Rustls];

    /// Whether this provider's feature is on in this build.
    #[must_use]
    pub const fn compiled(self) -> bool {
        match self {
            Provider::AwsLc => cfg!(feature = "aws-lc"),
            Provider::Ring => cfg!(feature = "ring"),
            Provider::Rustls => cfg!(feature = "rustls"),
        }
    }

    /// The first provider compiled in of AWS-LC, ring and rustls.
    #[must_use]
    pub const fn preferred() -> Provider {
        if cfg!(feature = "aws-lc") {
            Provider::AwsLc
        } else if cfg!(feature = "ring") {
            Provider::Ring
        } else {
            Provider::Rustls
        }
    }

    /// The provider's Cargo feature.
    #[must_use]
    pub const fn feature(self) -> &'static str {
        match self {
            Provider::AwsLc => "aws-lc",
            Provider::Ring => "ring",
            Provider::Rustls => "rustls",
        }
    }
}

impl Default for Provider {
    fn default() -> Provider {
        Provider::preferred()
    }
}
