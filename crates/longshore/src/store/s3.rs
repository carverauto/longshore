// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! S3 and S3-compatible object stores: `store`, `store_no_clobber`, `fetch`
//! and `erase` with SigV4 head signing and internal retries.
//!
//! Each operation takes a slot from a fixed table. An attempt is one signed
//! HTTP request; its outcome is classified and the operation is retried,
//! delayed, or finished. The caller sees only a final [`Outcome`].
//!
//! Bodies are never hashed: PUT declares `UNSIGNED-PAYLOAD`, so signing cost
//! does not depend on object size.

use crate::dns::Resolver;
use crate::http::client::{self, Client, Error, Handled, Request};
use crate::io::{BufId, Completion, FileId, Io, Op, Token};
use crate::store::creds::{Credentials, TOKEN_MAX};
use crate::store::multipart::{self, PartTag, UploadId, PART_NUMBER_MAX, UPLOAD_ID_MAX};
use crate::store::retry::{classify, Class, STATUS_PRECONDITION_FAILED};
use crate::store::sigv4::{self, authorization, uri_encode, EMPTY_SHA256, UNSIGNED_PAYLOAD};
use crate::store::time::amz_date;
use std::net::SocketAddrV4;

pub const KEY_ENCODED_MAX: usize = 512;
/// Room for a multipart query: `partNumber=10000&uploadId=` and an upload
/// id with every byte percent-encoded.
const QUERY_MAX: usize = 32 + 3 * UPLOAD_ID_MAX;
/// A credentials file must be smaller than this and than the HTTP buffer
/// size; a larger one is refused rather than read in part.
pub const CREDENTIALS_FILE_MAX: u32 = 8192;

const CHECKSUM_HEADER: &str = "x-amz-checksum-crc64nvme";

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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Provider {
    /// Waiting to reread at this time.
    Idle {
        at: u64,
    },
    Reading,
    /// A validation listing signed with the candidate is in progress.
    Validating,
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
    /// A part is stored. Keep the tag: completion sends it back.
    PartStored {
        tag: PartTag,
    },
    /// The upload is gone, by this call or already.
    Aborted,
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
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Store {
        no_clobber: bool,
    },
    Fetch {
        range: Option<(u64, u64)>,
    },
    Erase,
    List,
    /// Begin a multipart upload, asking for whole-object checksums if set.
    Begin {
        checksums: bool,
    },
    /// Store one part; its number is in the slot's query.
    Part,
    Complete,
    Abort,
}

impl Kind {
    /// The request carries the caller's bytes as an object or part of one.
    fn stores_bytes(self) -> bool {
        matches!(self, Kind::Store { .. } | Kind::Part)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Free,
    InFlight,
    /// Waiting to (re)try at this time.
    Waiting {
        until: u64,
    },
    /// Finished; the slot's `outcome` is waiting to be collected.
    Finished,
}

#[derive(Clone, Copy)]
struct Slot {
    state: State,
    /// The result, once `state` is `Finished`.
    outcome: Outcome,
    token: u64,
    kind: Kind,
    path: [u8; KEY_ENCODED_MAX],
    path_len: usize,
    /// Encoded query of a multipart request, in the sorted order signing
    /// requires. Empty for every other kind.
    query: [u8; QUERY_MAX],
    query_len: usize,
    body: Option<(BufId, u32)>,
    response_buf: Option<BufId>,
    attempts: u32,
    delays: u32,
    /// Times this operation waited for an address without starting an attempt.
    address_waits: u32,
    /// Sign with the candidate credentials (the validation request only).
    candidate: bool,
    /// A retried conditional store was refused as already existing: this
    /// attempt asks for the stored object's checksum to see whether an
    /// earlier attempt of the same operation is what created it.
    comparing: bool,
    /// CRC-64/NVME of the body, computed once and reused by every attempt.
    crc: Option<u64>,
}

pub struct S3 {
    cfg: Config,
    client: Client,
    /// `requests_max` slots for callers, plus one for credential validation.
    slots: Box<[Slot]>,
    /// The credentials in use are `creds[active]`; the other entry holds a
    /// candidate while it is being validated.
    creds: [Credentials; 2],
    active: usize,
    provider: Provider,
    /// Times new credentials were validated and adopted.
    pub credential_rotations: u64,
    /// Times the file could not be read or parsed, or its credentials were refused.
    pub credential_failures: u64,
    /// Times a retried conditional store was refused and the stored object's
    /// checksum was fetched to decide whose object it is.
    pub store_comparisons: u64,
}

impl S3 {
    /// End the operation in slot `i` with `outcome`.
    fn finish(&mut self, i: usize, outcome: Outcome) {
        self.slots[i].outcome = outcome;
        self.slots[i].state = State::Finished;
    }
}

mod attempt;
mod multipart_ops;
mod outcome;
mod rotation;
mod submit;

impl S3 {
    pub fn new(mut cfg: Config) -> S3 {
        assert!(cfg.attempts_max >= 1 && cfg.requests_max >= 1);
        // The signature's fixed buffers hold a region of this length with room to spare.
        assert!(cfg.region.len() <= 64, "region is longer than 64 bytes");
        let client = match cfg.tls.take() {
            Some((setup, first_buf)) => Client::with_tls(cfg.http, setup, first_buf),
            None => Client::new(cfg.http),
        };
        let slot = Slot {
            state: State::Free,
            outcome: Outcome::Erased,
            token: 0,
            kind: Kind::Erase,
            path: [0; KEY_ENCODED_MAX],
            path_len: 0,
            query: [0; QUERY_MAX],
            query_len: 0,
            body: None,
            response_buf: None,
            attempts: 0,
            delays: 0,
            address_waits: 0,
            candidate: false,
            comparing: false,
            crc: None,
        };
        let creds = [cfg.credentials; 2];
        S3 {
            client,
            slots: vec![slot; cfg.requests_max as usize + 1].into_boxed_slice(),
            creds,
            active: 0,
            provider: Provider::Idle { at: 0 },
            credential_rotations: 0,
            credential_failures: 0,
            store_comparisons: 0,
            cfg,
        }
    }

    /// Start background work (name resolution). Call once.
    pub fn start<IO: Io>(&mut self, io: &mut IO) {
        if let Endpoint::Resolved(r) = &mut self.cfg.endpoint {
            r.start(io);
        }
        if let Some(f) = self.cfg.credentials_file {
            self.provider = Provider::Idle {
                at: io.now_ns() + f.reread_ns,
            };
        }
    }

    /// Access key of the credentials currently in use.
    pub fn access_key(&self) -> &str {
        self.creds[self.active].access_key()
    }

    /// True when the endpoint address is known.
    pub fn ready(&self) -> bool {
        match &self.cfg.endpoint {
            Endpoint::Fixed(_) => true,
            Endpoint::Resolved(r) => r.ready(),
        }
    }

    /// HTTP connections opened so far.
    pub fn connects(&self) -> u64 {
        self.client.connects
    }

    /// Operations not yet collected.
    pub fn in_progress(&self) -> usize {
        self.slots[..self.cfg.requests_max as usize]
            .iter()
            .filter(|s| s.state != State::Free)
            .count()
    }

    /// Collect one finished operation, if any.
    pub fn poll(&mut self) -> Option<Done> {
        let s = self.slots[..self.cfg.requests_max as usize]
            .iter_mut()
            .find(|s| s.state == State::Finished)?;
        s.state = State::Free;
        Some(Done {
            token: s.token,
            outcome: s.outcome,
            attempts: s.attempts,
        })
    }

    fn wait<IO: Io>(&mut self, io: &mut IO, i: usize, ns: u64) {
        self.slots[i].state = State::Waiting {
            until: io.now_ns() + ns,
        };
    }

    /// Feed a completion. Returns false if it belongs to neither the HTTP
    /// client nor the resolver.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> bool {
        if let Some(f) = self.cfg.credentials_file.filter(|f| f.token == c.token.0) {
            if self.provider == Provider::Reading {
                self.on_credentials_read(io, f, c.result);
            }
            return true;
        }
        if let Endpoint::Resolved(r) = &mut self.cfg.endpoint {
            if r.on_completion(io, c) {
                return true;
            }
        }
        match self.client.on_completion(io, c) {
            Handled::NotMine => false,
            Handled::Pending => true,
            Handled::Done(d) => {
                self.on_attempt_done(io, d.token as usize, d.result);
                self.check_validation(io);
                true
            }
        }
    }

    /// Enforce request timeouts and start attempts whose wait has elapsed.
    /// Call at a fixed period.
    pub fn tick<IO: Io>(&mut self, io: &mut IO) {
        self.client.tick(io);
        let now = io.now_ns();
        if let (Some(f), Provider::Idle { at }) = (self.cfg.credentials_file, self.provider) {
            if now >= at {
                self.provider = Provider::Reading;
                let len = self.credentials_read_len();
                io.submit(
                    Token(f.token),
                    Op::ReadFile {
                        file: f.file,
                        buf: f.buf,
                        off: 0,
                        len,
                    },
                )
                .expect("Limits::ops_max must leave a slot for the credentials read");
            }
        }
        self.check_validation(io);
        for i in 0..self.slots.len() {
            if matches!(self.slots[i].state, State::Waiting { until } if now >= until) {
                self.attempt(io, i);
            }
        }
    }
}
