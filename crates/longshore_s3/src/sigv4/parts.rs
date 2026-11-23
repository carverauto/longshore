// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a [`Request`] and a [`Scope`] are made and read.

use super::{Request, Scope};

impl<'a> Request<'a> {
    /// The request to sign.
    #[inline]
    pub const fn new(
        method: &'a str,
        canonical_uri: &'a str,
        query: &'a [(&'a str, &'a str)],
        headers: &'a [(&'a str, &'a str)],
        payload_hash: &'a str,
    ) -> Self {
        Request {
            method,
            canonical_uri,
            query,
            headers,
            payload_hash,
        }
    }

    /// The request method, such as `GET`.
    #[inline]
    pub const fn method(&self) -> &'a str {
        self.method
    }

    /// Already URI-encoded path, e.g. from [`uri_encode`](super::uri_encode) with `encode_slash = false`.
    #[inline]
    pub const fn canonical_uri(&self) -> &'a str {
        self.canonical_uri
    }

    /// Already URI-encoded names and values, in any order.
    #[inline]
    pub const fn query(&self) -> &'a [(&'a str, &'a str)] {
        self.query
    }

    /// Headers to sign, in any order and any case. Must include `host`.
    #[inline]
    pub const fn headers(&self) -> &'a [(&'a str, &'a str)] {
        self.headers
    }

    /// Value for the last line of the canonical request.
    #[inline]
    pub const fn payload_hash(&self) -> &'a str {
        self.payload_hash
    }
}

impl<'a> Scope<'a> {
    /// Who signs, where, for which service and when.
    #[inline]
    pub const fn new(
        access_key: &'a str,
        secret_key: &'a str,
        region: &'a str,
        service: &'a str,
        amz_date: &'a [u8; 16],
    ) -> Self {
        Scope {
            access_key,
            secret_key,
            region,
            service,
            amz_date,
        }
    }

    /// The access key id.
    #[inline]
    pub const fn access_key(&self) -> &'a str {
        self.access_key
    }

    /// The secret access key.
    #[inline]
    pub const fn secret_key(&self) -> &'a str {
        self.secret_key
    }

    /// Region used in the signature scope.
    #[inline]
    pub const fn region(&self) -> &'a str {
        self.region
    }

    /// The service signed for, such as `s3`.
    #[inline]
    pub const fn service(&self) -> &'a str {
        self.service
    }

    /// `YYYYMMDDTHHMMSSZ`.
    #[inline]
    pub const fn amz_date(&self) -> &'a [u8; 16] {
        self.amz_date
    }
}
