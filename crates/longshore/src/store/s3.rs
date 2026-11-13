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

use crate::http::client::{self, Client, Error, Handled, Request};
use crate::io::{BufId, Completion, Io, Op, Token};
use crate::store::creds::{Credentials, TOKEN_MAX};
use crate::store::multipart::{self, PartTag, UploadId, PART_NUMBER_MAX, UPLOAD_ID_MAX};
use crate::store::options::{Options, NAME_MAX, OPTIONS_MAX};
use crate::store::retry::{classify, Class, STATUS_PRECONDITION_FAILED};
use crate::store::sigv4::{self, authorization, uri_encode, EMPTY_SHA256, UNSIGNED_PAYLOAD};
use crate::store::stat::Stat;
use crate::store::time::{amz_date, parse_http_date};
use std::net::SocketAddr;

pub const KEY_ENCODED_MAX: usize = 512;
/// A bucket name's longest length.
pub const BUCKET_MAX: usize = 63;
/// A request path: the bucket when it is addressed there, then the key.
const PATH_MAX: usize = 1 + BUCKET_MAX + KEY_ENCODED_MAX;
/// The longest continuation token a listing can be resumed with, once
/// percent-encoded.
pub const LIST_TOKEN_ENCODED_MAX: usize = 1024;
/// A slot's request target: a path, or a listing's query with a token.
const TARGET_MAX: usize = PATH_MAX + LIST_TOKEN_ENCODED_MAX;
/// Room for a multipart query: `partNumber=10000&uploadId=` and an upload
/// id with every byte percent-encoded.
const QUERY_MAX: usize = 32 + 3 * UPLOAD_ID_MAX;
/// A credentials file must be smaller than this and than the HTTP buffer
/// size; a larger one is refused rather than read in part.
pub const CREDENTIALS_FILE_MAX: u32 = 8192;

const CHECKSUM_HEADER: &str = "x-amz-checksum-crc64nvme";

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
    /// Ask about an object without fetching it.
    Stat,
    /// Copy inside the store; the source is in the slot's query field.
    Copy,
    /// Erase the keys named in the body.
    EraseMany,
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
    path: [u8; TARGET_MAX],
    path_len: usize,
    /// Encoded query of a multipart request, in the sorted order signing
    /// requires. For a copy, the source as its header carries it. Empty
    /// for every other kind.
    query: [u8; QUERY_MAX],
    query_len: usize,
    /// Optional headers of a store or of beginning an upload.
    options: Options,
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
    /// What every request path starts with: empty, or `/bucket` when the
    /// bucket is addressed in the path.
    root: String,
    cfg: Config,
    client: Client,
    /// `requests_max` slots for callers, plus one for credential validation.
    slots: Box<[Slot]>,
    /// The credentials in use are `creds[active]`; the other entry holds a
    /// candidate while it is being validated.
    creds: [Credentials; 2],
    active: usize,
    provider: Provider,
    /// Connections use TLS.
    secure: bool,
    /// Times new credentials were validated and adopted.
    pub credential_rotations: u64,
    /// Times the file could not be read or parsed, or its credentials were refused.
    pub credential_failures: u64,
    /// Times a retried conditional store was refused and the stored object's
    /// checksum was fetched to decide whose object it is.
    pub store_comparisons: u64,
    /// Connections retired because their address's segment size ranked too
    /// small, when the resolver prefers large segments.
    pub small_segment_retired: u64,
    /// The resolver's cycle count when its answer was last acted on.
    resolver_cycles: u64,
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
mod object_ops;
mod outcome;
mod rotation;
mod submit;
mod types;
pub use types::*;

impl S3 {
    pub fn new(mut cfg: Config) -> S3 {
        assert!(cfg.attempts_max >= 1 && cfg.requests_max >= 1);
        // The signature's fixed buffers hold a region of this length with room to spare.
        assert!(cfg.region.len() <= 64, "region is longer than 64 bytes");
        // What every request path starts with: nothing, or "/bucket".
        let root = match &cfg.bucket_in_path {
            Some(bucket) => {
                let valid = (3..=BUCKET_MAX).contains(&bucket.len())
                    && bucket.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'
                    });
                assert!(valid, "bucket_in_path is not a bucket name: {bucket:?}");
                format!("/{bucket}")
            }
            None => String::new(),
        };
        let secure = cfg.tls.is_some();
        let client = match cfg.tls.take() {
            Some((setup, first_buf)) => Client::with_tls(cfg.http, setup, first_buf),
            None => Client::new(cfg.http),
        };
        let slot = Slot {
            state: State::Free,
            outcome: Outcome::Erased,
            token: 0,
            kind: Kind::Erase,
            path: [0; TARGET_MAX],
            path_len: 0,
            query: [0; QUERY_MAX],
            query_len: 0,
            options: Options::new(),
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
            root,
            client,
            slots: vec![slot; cfg.requests_max as usize + 1].into_boxed_slice(),
            creds,
            secure,
            active: 0,
            provider: Provider::Idle { at: 0 },
            credential_rotations: 0,
            credential_failures: 0,
            store_comparisons: 0,
            small_segment_retired: 0,
            resolver_cycles: 0,
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

    /// Turn the preference for addresses with large TCP segments on or off
    /// while the store runs. `Some(keep)` has the I/O backend measure each
    /// new connection, and the resolver hand out the large-segment
    /// addresses, keeping at least `keep` of them in use. `None` turns both
    /// off. Turning it on replaces each open connection the next time it
    /// would be reused, if it was never measured or measured small, so the
    /// change takes hold within about one request per connection. With a
    /// fixed endpoint there is nothing to choose between and this does
    /// nothing.
    pub fn prefer_large_segments<IO: Io>(&mut self, io: &mut IO, keep: Option<usize>) {
        if let Endpoint::Resolved(r) = &mut self.cfg.endpoint {
            io.measure_segment_sizes(keep.is_some());
            r.set_prefer_large_segments(keep);
            if keep.is_some() {
                self.client.retire_unmeasured(0);
                let floor = r.segment_floor(0);
                self.small_segment_retired += self.client.retire_below(0, floor) as u64;
            }
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
                // A new answer: replace connections it ranks too small, and
                // open one to each address worth measuring.
                if r.cycles != self.resolver_cycles {
                    self.resolver_cycles = r.cycles;
                    let floor = r.segment_floor(0);
                    self.small_segment_retired += self.client.retire_below(0, floor) as u64;
                    self.client.retire_some(0, r.probes_wanted(0) as u32);
                }
                return true;
            }
        }
        let handled = self.client.on_completion(io, c);
        // An address that cannot be connected to: let the resolver move to
        // the other family if the name has one.
        if let Some(addr) = self.client.connect_failure.take() {
            if let Endpoint::Resolved(r) = &mut self.cfg.endpoint {
                r.connect_failed(0, addr.ip());
            }
        }
        // A measured connection: tell the resolver its address's segment
        // size, and stop reusing connections it now ranks too small.
        if let Some((addr, segment)) = self.client.connected.take() {
            if let Endpoint::Resolved(r) = &mut self.cfg.endpoint {
                let floor = r.connected(0, addr.ip(), segment);
                self.small_segment_retired += self.client.retire_below(0, floor) as u64;
            }
        }
        match handled {
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
