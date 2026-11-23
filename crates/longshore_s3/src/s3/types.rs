// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a caller configures the store with and what it reports back.

use crate::creds::Credentials;
use crate::multipart::{PartTag, UploadId};
use crate::report::Report;
use crate::stat::Stat;
use longshore_common::io::{BufId, FileId};
use longshore_dns::Resolver;
use longshore_http_client::client::{self, Error};
use std::net::SocketAddr;

/// Integrity checking for a store.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Checksum {
    /// Send no checksum.
    None,
    /// Compute CRC-64/NVME over the body and send it. One extra pass over the data.
    Compute,
    /// The caller already has the CRC-64/NVME of the body.
    Provided(u64),
    /// The caller has the SHA-256 of the body, and it is sent in place of
    /// a CRC-64/NVME: a store takes one kind of checksum per object. The
    /// store checks the body against it; this client never hashes a body
    /// with SHA-256. For a whole object on S3 only: a part of an upload or
    /// a store on Google Cloud Storage is refused as
    /// [`SubmitError::Unsupported`].
    Sha256([u8; 32]),
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

/// How a store is reached and how it behaves. Made with [`Config::new`]
/// and the `with_*` setters.
pub struct Config {
    pub(crate) vendor: Vendor,
    pub(crate) host: String,
    pub(crate) bucket_in_path: Option<String>,
    pub(crate) port: u16,
    pub(crate) region: String,
    pub(crate) credentials: Credentials,
    pub(crate) credentials_file: Option<CredentialsFile>,
    pub(crate) requests_max: u32,
    pub(crate) attempts_max: u32,
    pub(crate) backoff_ns: u64,
    pub(crate) backoff_max_ns: u64,
    pub(crate) require_checksums: bool,
    pub(crate) http: client::ClientConfig,
    #[cfg(feature = "tls")]
    pub(crate) tls: Option<(longshore_tls::handshake::ClientSetup, u32)>,
    pub(crate) endpoint: Endpoint,
}

/// A credentials file to watch. It is reread every `reread_ns`. When its
/// contents differ from the credentials in use, the new ones are tried with
/// a one-key listing and adopted only if the store accepts them.
#[derive(Clone, Copy, Debug)]
pub struct CredentialsFile {
    pub(crate) file: FileId,
    pub(crate) buf: BufId,
    pub(crate) reread_ns: u64,
    pub(crate) token: u64,
    pub(crate) validate: Validate,
}

/// The request that proves new credentials before they are adopted.
#[expect(
    clippy::large_enum_variant,
    reason = "the prefix is held inline so the setting stays `Copy` and allocates nothing; there is one per store"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validate {
    /// List one key under this prefix. The credentials must be allowed to
    /// list there: a policy that permits listing only under some prefix
    /// refuses a listing of the whole bucket, and the new credentials would
    /// then never be adopted. [`KeyPrefix::EMPTY`] lists the whole bucket.
    List(KeyPrefix),
    /// Send nothing and adopt what the file holds. For credentials that may
    /// not list at all. Wrong credentials are then in use until the file is
    /// corrected and reread.
    Nothing,
}

/// A key prefix of at most [`KeyPrefix::MAX`] bytes, held without allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPrefix {
    bytes: [u8; KeyPrefix::MAX],
    len: u16,
}

impl KeyPrefix {
    pub const MAX: usize = 256;
    /// No prefix: the whole bucket.
    pub const EMPTY: KeyPrefix = KeyPrefix {
        bytes: [0; KeyPrefix::MAX],
        len: 0,
    };

    /// `None` if `prefix` is longer than [`KeyPrefix::MAX`] bytes.
    pub fn new(prefix: &str) -> Option<KeyPrefix> {
        let mut p = KeyPrefix::EMPTY;
        p.bytes
            .get_mut(..prefix.len())?
            .copy_from_slice(prefix.as_bytes());
        p.len = prefix.len() as u16;
        Some(p)
    }

    pub fn as_str(&self) -> &str {
        crate::text::written(&self.bytes[..self.len as usize])
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "one per store, built once and never moved while it runs, so the size difference between the variants costs nothing"
)]
pub enum Endpoint {
    Fixed(SocketAddr),
    Resolved(Resolver),
}

#[expect(
    clippy::large_enum_variant,
    reason = "held in slots that exist whether or not they are used, and passed by value: boxing the large variant would allocate on every operation"
)]
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
    /// [`crate::list::Listing`].
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
    /// [`crate::erase::Failures`].
    ErasedMany {
        len: u32,
    },
    /// How a `put`, `head` or `get` ended. They end no other way.
    Report(Report),
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
    pub(crate) token: u64,
    pub(crate) outcome: Outcome,
    pub(crate) attempts: u32,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SubmitError {
    /// All operation slots are in use.
    Busy,
    /// The key is empty or too long once encoded, or an expected `ETag`
    /// holds a byte that is not visible ASCII and so cannot be sent.
    BadKey,
    /// The byte range ends before it starts.
    BadRange,
    /// A part number outside 1 to 10000.
    BadPart,
    /// This store has no such operation.
    Unsupported,
}

/// The bucket named by a host of the form `bucket.endpoint`: what is left
/// when the store's endpoint is taken off the right.
///
/// A bucket's name may contain dots, so it is not simply the first label.
/// The endpoint is recognised by what marks it: for S3 and stores like it,
/// the last label that is `s3` or begins `s3-`; for Google Cloud Storage,
/// `storage.googleapis.com`. A host with no such mark gives its first
/// label, which is right only for a bucket without dots; a dotted bucket
/// on such a store must be addressed in the path instead.
pub fn bucket_in_host(vendor: Vendor, host: &str) -> &str {
    let first = host.split('.').next().unwrap_or("");
    match vendor {
        Vendor::Gcs => host
            .strip_suffix(".storage.googleapis.com")
            .unwrap_or(first),
        Vendor::S3 => {
            // Byte offset of the last endpoint label that is not the first.
            let mut at = 0;
            let mut endpoint = None;
            for label in host.split('.') {
                if at > 0 && (label == "s3" || label.starts_with("s3-")) {
                    endpoint = Some(at);
                }
                at += label.len() + 1;
            }
            endpoint.map_or(first, |offset| &host[..offset - 1])
        }
    }
}

impl CredentialsFile {
    /// Watch `file`, rereading it into `buf` every `reread_ns` with `token`, and try new credentials as `validate` says.
    #[inline]
    pub const fn new(
        file: FileId,
        buf: BufId,
        reread_ns: u64,
        token: u64,
        validate: Validate,
    ) -> Self {
        CredentialsFile {
            file,
            buf,
            reread_ns,
            token,
            validate,
        }
    }

    /// The registered file holding the credentials.
    #[inline]
    pub const fn file(self) -> FileId {
        self.file
    }

    /// A pool buffer for reading the file and for the validation listing.
    #[inline]
    pub const fn buf(self) -> BufId {
        self.buf
    }

    /// How often the file is reread.
    #[inline]
    pub const fn reread_ns(self) -> u64 {
        self.reread_ns
    }

    /// `Io` token for the file read.
    #[inline]
    pub const fn token(self) -> u64 {
        self.token
    }

    /// How new credentials are tried before they replace the ones in use.
    #[inline]
    pub const fn validate(self) -> Validate {
        self.validate
    }
}

impl Done {
    /// Operation `token` ended with `outcome` after `attempts` requests.
    #[inline]
    pub const fn new(token: u64, outcome: Outcome, attempts: u32) -> Done {
        Done {
            token,
            outcome,
            attempts,
        }
    }

    /// The operation's token, as submitted.
    #[inline]
    pub const fn token(self) -> u64 {
        self.token
    }

    /// How the operation ended.
    #[inline]
    pub const fn outcome(self) -> Outcome {
        self.outcome
    }

    /// Requests the operation sent.
    #[inline]
    pub const fn attempts(self) -> u32 {
        self.attempts
    }
}
