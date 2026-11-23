// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a store's [`Config`] is made, changed and read.

use super::{Config, CredentialsFile, Endpoint, Vendor};
use crate::creds::Credentials;
use longshore_http_client::client::ClientConfig;

/// Nanoseconds in a millisecond.
const MS: u64 = 1_000_000;

impl Config {
    /// A store of `vendor` at `host` in `region`, signed for with
    /// `credentials`, reached at `endpoint` over connections made as `http`
    /// says. The rest starts as the client's own stores do: port 80, the
    /// bucket in the host name, no credentials file, 64 operations at once,
    /// 4 attempts each, a backoff of 200 ms doubling up to 5 s, checksums
    /// not required, and no TLS. The `with_*` setters change it.
    pub fn new(
        vendor: Vendor,
        host: String,
        region: String,
        credentials: Credentials,
        http: ClientConfig,
        endpoint: Endpoint,
    ) -> Config {
        Config {
            vendor,
            host,
            bucket_in_path: None,
            port: 80,
            region,
            credentials,
            credentials_file: None,
            requests_max: 64,
            attempts_max: 4,
            backoff_ns: 200 * MS,
            backoff_max_ns: 5_000 * MS,
            require_checksums: false,
            http,
            #[cfg(feature = "tls")]
            tls: None,
            endpoint,
        }
    }

    /// Which store's request forms to use.
    #[inline]
    pub fn vendor(&self) -> Vendor {
        self.vendor
    }

    /// This configuration speaking to a store of `vendor`.
    #[must_use]
    pub fn with_vendor(mut self, vendor: Vendor) -> Config {
        self.vendor = vendor;
        self
    }

    /// Host header and DNS name. With the bucket in the host name, which is
    /// the default, this is `bucket.endpoint`. With
    /// [`Config::bucket_in_path`] it is the endpoint's own name.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// This configuration with `host` as the Host header and DNS name.
    #[must_use]
    pub fn with_host(mut self, host: String) -> Config {
        self.host = host;
        self
    }

    /// `Some(bucket)` addresses the bucket in the path: every request goes
    /// to `/bucket/key` on `host`. Self-hosted S3-compatible stores commonly
    /// expect this. `None` addresses it in the host name.
    pub fn bucket_in_path(&self) -> Option<&str> {
        self.bucket_in_path.as_deref()
    }

    /// This configuration addressing the bucket as `bucket_in_path` says.
    #[must_use]
    pub fn with_bucket_in_path(mut self, bucket_in_path: Option<String>) -> Config {
        self.bucket_in_path = bucket_in_path;
        self
    }

    /// The port connected to.
    #[inline]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// This configuration connecting to `port`.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Config {
        self.port = port;
        self
    }

    /// Region used in the signature scope.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// This configuration signing for `region`.
    #[must_use]
    pub fn with_region(mut self, region: String) -> Config {
        self.region = region;
        self
    }

    /// Credentials to start with.
    #[inline]
    pub fn credentials(&self) -> Credentials {
        self.credentials
    }

    /// This configuration starting with `credentials`.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Credentials) -> Config {
        self.credentials = credentials;
        self
    }

    /// With `Some`, credentials are reread from a file and rotated in.
    #[inline]
    pub fn credentials_file(&self) -> Option<CredentialsFile> {
        self.credentials_file
    }

    /// This configuration rereading credentials as `credentials_file` says.
    #[must_use]
    pub fn with_credentials_file(mut self, credentials_file: Option<CredentialsFile>) -> Config {
        self.credentials_file = credentials_file;
        self
    }

    /// Operations in progress at most.
    #[inline]
    pub fn requests_max(&self) -> u32 {
        self.requests_max
    }

    /// This configuration running at most `requests_max` operations at once.
    #[must_use]
    pub fn with_requests_max(mut self, requests_max: u32) -> Config {
        self.requests_max = requests_max;
        self
    }

    /// Attempts per operation, including the first.
    #[inline]
    pub fn attempts_max(&self) -> u32 {
        self.attempts_max
    }

    /// This configuration making at most `attempts_max` attempts per operation.
    #[must_use]
    pub fn with_attempts_max(mut self, attempts_max: u32) -> Config {
        self.attempts_max = attempts_max;
        self
    }

    /// First delay after a slow-down response; doubles up to
    /// [`Config::backoff_max_ns`].
    #[inline]
    pub fn backoff_ns(&self) -> u64 {
        self.backoff_ns
    }

    /// This configuration first waiting `backoff_ns` after a slow-down response.
    #[must_use]
    pub fn with_backoff_ns(mut self, backoff_ns: u64) -> Config {
        self.backoff_ns = backoff_ns;
        self
    }

    /// The longest delay after a slow-down response.
    #[inline]
    pub fn backoff_max_ns(&self) -> u64 {
        self.backoff_max_ns
    }

    /// This configuration waiting at most `backoff_max_ns` after a slow-down response.
    #[must_use]
    pub fn with_backoff_max_ns(mut self, backoff_max_ns: u64) -> Config {
        self.backoff_max_ns = backoff_max_ns;
        self
    }

    /// The store is known to return a checksum with every whole-object
    /// fetch. A response without a well-formed one is then treated as
    /// damaged and fetched again, instead of being accepted unverified.
    #[inline]
    pub fn require_checksums(&self) -> bool {
        self.require_checksums
    }

    /// This configuration requiring checksums as `require_checksums` says.
    #[must_use]
    pub fn with_require_checksums(mut self, require_checksums: bool) -> Config {
        self.require_checksums = require_checksums;
        self
    }

    /// How the HTTP client's connections are made.
    #[inline]
    pub fn http(&self) -> ClientConfig {
        self.http
    }

    /// This configuration making connections as `http` says.
    #[must_use]
    pub fn with_http(mut self, http: ClientConfig) -> Config {
        self.http = http;
        self
    }

    /// With `Some`, connections use TLS: the handshake setup, and the first
    /// of the `2 * http.connections_max` pool buffers the record layer uses.
    #[cfg(feature = "tls")]
    pub fn tls(&self) -> Option<&(longshore_tls::handshake::ClientSetup, u32)> {
        self.tls.as_ref()
    }

    /// This configuration using TLS as `tls` says.
    #[cfg(feature = "tls")]
    #[must_use]
    pub fn with_tls(mut self, tls: Option<(longshore_tls::handshake::ClientSetup, u32)>) -> Config {
        self.tls = tls;
        self
    }

    /// Where the store is. With `Resolved`, host index 0 of the resolver is used.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// This configuration reaching the store at `endpoint`.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: Endpoint) -> Config {
        self.endpoint = endpoint;
        self
    }
}
