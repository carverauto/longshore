// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a caller configures the store with and what it reports back.

use crate::dns::Resolver;
use crate::http::client::{self, Error};
use crate::io::{BufId, FileId};
use crate::store::creds::Credentials;
use crate::store::multipart::{PartTag, UploadId};
use crate::store::stat::Stat;
use std::net::SocketAddrV4;

/// Integrity checking for a store.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Checksum {
    /// Send no checksum.
    None,
    /// Compute CRC-64/NVME over the body and send it. One extra pass over the data.
    Compute,
    /// The caller already has the CRC-64/NVME of the body.
    Provided(u64),
}

/// Which store's request forms to use.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Vendor {
    /// S3 and compatible stores: SigV4 signatures, CRC-64/NVME checksums,
    /// `If-None-Match: *`.
    S3,
    /// Google Cloud Storage through its XML API: a bearer token instead of a
    /// signature, CRC-32C in `x-goog-hash`, `x-goog-if-generation-match: 0`.
    /// Credentials are [`Credentials::bearer`]; a credentials file holds
    /// the token alone.
    Gcs,
}

pub struct Config {
    pub vendor: Vendor,
    /// Host header and DNS name: `bucket.endpoint` (virtual-hosted style).
    pub host: String,
    pub port: u16,
    /// Region used in the signature scope.
    pub region: String,
    /// Credentials to start with.
    pub credentials: Credentials,
    /// With `Some`, credentials are reread from a file and rotated in.
    pub credentials_file: Option<CredentialsFile>,
    /// Operations in progress at most.
    pub requests_max: u32,
    /// Attempts per operation, including the first.
    pub attempts_max: u32,
    /// First delay after a slow-down response; doubles up to `backoff_max_ns`.
    pub backoff_ns: u64,
    pub backoff_max_ns: u64,
    /// The store is known to return a checksum with every whole-object
    /// fetch. A response without a well-formed one is then treated as
    /// damaged and fetched again, instead of being accepted unverified.
    pub require_checksums: bool,
    pub http: client::Config,
    /// With `Some`, connections use TLS: the handshake setup, and the first
    /// of the `2 * http.connections_max` pool buffers the record layer uses.
    pub tls: Option<(crate::tls::handshake::ClientSetup, u32)>,
    /// Where the store is. With `Resolved`, host index 0 of the resolver is used.
    pub endpoint: Endpoint,
}

/// A credentials file to watch. It is reread every `reread_ns`. When its
/// contents differ from the credentials in use, the new ones are tried with
/// a one-key listing and adopted only if the store accepts them.
#[derive(Clone, Copy, Debug)]
pub struct CredentialsFile {
    pub file: FileId,
    /// A pool buffer for reading the file and for the validation listing.
    pub buf: BufId,
    pub reread_ns: u64,
    /// `Io` token for the file read.
    pub token: u64,
}

pub enum Endpoint {
    Fixed(SocketAddrV4),
    Resolved(Resolver),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Outcome {
    Stored,
    /// `store_no_clobber` found an object at the key. If `after_retry`, an
    /// earlier attempt of this same operation may be what created it.
    AlreadyExists {
        after_retry: bool,
    },
    /// `verified` is true when the store returned a checksum and the bytes
    /// received match it. Ranged fetches and stores that return no checksum
    /// are not verified.
    Fetched {
        len: u32,
        verified: bool,
    },
    /// The listing document is in the caller's buffer; parse it with
    /// [`crate::store::list::Listing`].
    Listed {
        len: u32,
    },
    NotFound,
    Erased,
    /// A multipart upload has begun. Keep the id: every later call needs it.
    Begun {
        upload: UploadId,
    },
    /// A part is stored. Keep the tag and checksum: completion sends them
    /// back. `checksum` is the CRC-64/NVME the part was sent with on S3;
    /// `None` without one, and always on Google Cloud Storage.
    PartStored {
        tag: PartTag,
        checksum: Option<u64>,
    },
    /// The upload is gone, by this call or already.
    Aborted,
    /// What the store knows about the object.
    Stat(Stat),
    /// The object was copied inside the store.
    Copied,
    /// The batch was carried out. The store's report, `len` bytes, is in the
    /// caller's buffer: read the keys it could not erase with
    /// [`crate::store::erase::Failures`].
    ErasedMany {
        len: u32,
    },
    Fatal(Fatal),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Fatal {
    /// The store answered with a status that will not improve on retry.
    Status(u16),
    Transport(Error),
    /// Every attempt failed retryably.
    RetriesExhausted,
    /// Every attempt of a fetch returned bytes that did not match the
    /// store's checksum.
    ChecksumMismatch,
    /// The store's name never resolved to an address.
    NoAddress,
    /// A ranged fetch was answered with the whole object instead of the
    /// range. The bytes received are not the bytes asked for.
    RangeIgnored,
    /// A success response lacked what it must carry: an upload id, or a
    /// part's tag, or one longer than its fixed-size type holds.
    BadResponse,
    /// The store answered with status 200 and an error document that is not
    /// worth retrying.
    ErrorDocument,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Done {
    pub token: u64,
    pub outcome: Outcome,
    pub attempts: u32,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SubmitError {
    /// All operation slots are in use.
    Busy,
    /// The key is empty or too long once encoded.
    BadKey,
    /// The byte range ends before it starts.
    BadRange,
    /// A part number outside 1 to 10000.
    BadPart,
    /// This store has no such operation.
    Unsupported,
}
