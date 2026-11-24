// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Client and server configuration, built once at initialization, each for
//! one [`Provider`].

use super::TlsError;
use crate::provider::Provider;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use std::sync::Arc;

#[cfg(feature = "aws-lc")]
fn aws_lc() -> Option<CryptoProvider> {
    Some(rustls::crypto::aws_lc_rs::default_provider())
}

#[cfg(not(feature = "aws-lc"))]
fn aws_lc() -> Option<CryptoProvider> {
    None
}

#[cfg(any(feature = "ring", feature = "rustls"))]
fn ring() -> Option<CryptoProvider> {
    Some(rustls::crypto::ring::default_provider())
}

#[cfg(not(any(feature = "ring", feature = "rustls")))]
fn ring() -> Option<CryptoProvider> {
    None
}

/// TLS 1.3 with the two AES-GCM suites and nothing else, AES-128 first, from
/// rustls's provider for `provider`: AWS-LC's, or ring's for both the ring
/// and the rustls record layers. With `aes128_only`, just the suite every
/// TLS 1.3 peer must implement. Fails when the provider is not compiled in.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "rustls::CipherSuite is foreign and non_exhaustive: every other suite is dropped"
)]
fn crypto(provider: Provider, aes128_only: bool) -> Result<Arc<CryptoProvider>, TlsError> {
    if !provider.compiled() {
        return Err(TlsError::Setup);
    }
    let mut p = match provider {
        Provider::AwsLc => aws_lc(),
        Provider::Ring | Provider::Rustls => ring(),
    }
    .ok_or(TlsError::Setup)?;
    p.cipher_suites.retain(|s| match s.suite() {
        rustls::CipherSuite::TLS13_AES_128_GCM_SHA256 => true,
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384 => !aes128_only,
        _ => false,
    });
    p.cipher_suites
        .sort_by_key(|s| s.suite() != rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    Ok(Arc::new(p))
}

fn public_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

/// Client configuration, built once at initialization.
pub struct ClientSetup(Arc<ClientConfig>, Provider);

impl ClientSetup {
    /// Trust the Mozilla root set compiled in through `webpki-roots`, with
    /// the preferred provider ([`Provider::preferred`]).
    pub fn with_public_roots() -> ClientSetup {
        Self::preferred(public_roots(), false)
    }

    /// As [`ClientSetup::with_public_roots`], but offering only AES-128-GCM,
    /// the faster of the two suites and the one TLS 1.3 requires every peer
    /// to support.
    pub fn with_public_roots_aes128() -> ClientSetup {
        Self::preferred(public_roots(), true)
    }

    /// Trust exactly these DER-encoded certificates as roots, with the
    /// preferred provider.
    pub fn with_roots(der: &[&[u8]]) -> Result<ClientSetup, TlsError> {
        Self::with_roots_using(Provider::preferred(), der)
    }

    /// The public roots with `provider`; with `aes128_only`, offering only
    /// AES-128-GCM. Fails when the provider is not compiled in.
    pub fn with_public_roots_using(
        provider: Provider,
        aes128_only: bool,
    ) -> Result<ClientSetup, TlsError> {
        Self::build(provider, public_roots(), aes128_only)
    }

    /// These DER-encoded certificates as roots, with `provider`. Fails when
    /// a certificate is unusable or the provider is not compiled in.
    pub fn with_roots_using(provider: Provider, der: &[&[u8]]) -> Result<ClientSetup, TlsError> {
        let mut roots = RootCertStore::empty();
        for cert in der {
            roots
                .add(CertificateDer::from(cert.to_vec()))
                .map_err(|_| TlsError::Setup)?;
        }
        Self::build(provider, roots, false)
    }

    /// The provider whose handshake and records this configuration uses.
    pub fn provider(&self) -> Provider {
        self.1
    }

    pub(super) fn config(&self) -> Arc<ClientConfig> {
        self.0.clone()
    }

    #[expect(
        clippy::expect_used,
        reason = "the preferred provider is compiled in and supports TLS 1.3"
    )]
    fn preferred(roots: RootCertStore, aes128_only: bool) -> ClientSetup {
        Self::build(Provider::preferred(), roots, aes128_only)
            .expect("the preferred provider is compiled in")
    }

    fn build(
        provider: Provider,
        roots: RootCertStore,
        aes128_only: bool,
    ) -> Result<ClientSetup, TlsError> {
        let mut config = ClientConfig::builder_with_provider(crypto(provider, aes128_only)?)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| TlsError::Setup)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.enable_secret_extraction = true;
        // No tickets are stored: every connection does a full handshake.
        config.resumption = rustls::client::Resumption::disabled();
        Ok(ClientSetup(Arc::new(config), provider))
    }
}

/// Server configuration for the in-tree test server.
pub struct ServerSetup(Arc<ServerConfig>, Provider);

impl ServerSetup {
    /// `chain` is the certificate chain, leaf first; `key` is its PKCS#8
    /// private key. Both DER. The preferred provider.
    pub fn new(chain: &[&[u8]], key: &[u8]) -> Result<ServerSetup, TlsError> {
        Self::new_using(Provider::preferred(), chain, key)
    }

    /// As [`ServerSetup::new`], with `provider`. Fails when the provider is
    /// not compiled in.
    pub fn new_using(
        provider: Provider,
        chain: &[&[u8]],
        key: &[u8],
    ) -> Result<ServerSetup, TlsError> {
        let chain: Vec<CertificateDer<'static>> = chain
            .iter()
            .map(|c| CertificateDer::from(c.to_vec()))
            .collect();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
        let mut config = ServerConfig::builder_with_provider(crypto(provider, false)?)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| TlsError::Setup)?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|_| TlsError::Setup)?;
        config.enable_secret_extraction = true;
        Ok(ServerSetup(Arc::new(config), provider))
    }

    /// The provider whose handshake and records this configuration uses.
    pub fn provider(&self) -> Provider {
        self.1
    }

    pub(super) fn config(&self) -> Arc<ServerConfig> {
        self.0.clone()
    }
}
