// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters and `with_*` setters for what `put`, `head` and `get` ask: [`Put`], [`Expect`] and [`Limit`].

use super::{ETag, Encryption, Expect, Limit, Put, VersionId};
use crate::s3::Checksum;

impl Put {
    /// Store only if no object exists at the key: `If-None-Match: *`.
    #[inline]
    pub const fn no_clobber(self) -> bool {
        self.no_clobber
    }

    /// This value with [`Put::no_clobber`] set to `no_clobber`.
    #[inline]
    pub const fn with_no_clobber(mut self, no_clobber: bool) -> Self {
        self.no_clobber = no_clobber;
        self
    }

    /// Server-side encryption to ask for, if any.
    #[inline]
    pub const fn encryption(self) -> Option<Encryption> {
        self.encryption
    }

    /// This value with [`Put::encryption`] set to `encryption`.
    #[inline]
    pub const fn with_encryption(mut self, encryption: Option<Encryption>) -> Self {
        self.encryption = encryption;
        self
    }

    /// The checksum the body is sent with. A store takes one kind per
    /// object, so [`Checksum::Sha256`] is sent instead of a CRC-64/NVME,
    /// not beside it.
    #[inline]
    pub const fn checksum(self) -> Checksum {
        self.checksum
    }

    /// This value with [`Put::checksum`] set to `checksum`.
    #[inline]
    pub const fn with_checksum(mut self, checksum: Checksum) -> Self {
        self.checksum = checksum;
        self
    }
}
impl Expect {
    /// Sent as `If-Match`: the object must have this `ETag`.
    #[inline]
    pub const fn etag(self) -> Option<ETag> {
        self.etag
    }

    /// This value with [`Expect::etag`] set to `etag`.
    #[inline]
    pub const fn with_etag(mut self, etag: Option<ETag>) -> Self {
        self.etag = etag;
        self
    }

    /// Sent as `versionId`: this version of the object is the one asked about.
    #[inline]
    pub const fn version(self) -> Option<VersionId> {
        self.version
    }

    /// This value with [`Expect::version`] set to `version`.
    #[inline]
    pub const fn with_version(mut self, version: Option<VersionId>) -> Self {
        self.version = version;
        self
    }
}
impl Limit {
    /// At most `attempts` requests, none started at or after `deadline_ns`.
    #[inline]
    pub const fn new(attempts: u32, deadline_ns: Option<u64>) -> Self {
        Limit {
            attempts,
            deadline_ns,
        }
    }

    /// Requests the operation may send, counting the first. Zero is read
    /// as one, and more than the store's own limit as that limit.
    #[inline]
    pub const fn attempts(self) -> u32 {
        self.attempts
    }

    /// On the clock of `Io::now_ns`. No request is started at or after
    /// this time, and one in progress is cancelled at the store's first
    /// `tick` at or after it.
    #[inline]
    pub const fn deadline_ns(self) -> Option<u64> {
        self.deadline_ns
    }
}
