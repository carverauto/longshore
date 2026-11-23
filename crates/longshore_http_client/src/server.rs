// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A small HTTP/1.1 object server over `Io`, for tests and benchmarks.
//!
//! It stores objects by request target (PUT, GET, HEAD, DELETE) and can be
//! scripted to misbehave: wrong statuses, stalls, truncated bodies, garbage.
//! Because it is generic over `Io`, the client and this server run together
//! on one simulated `Io`, or on real sockets.

use crate::head::{parse_request, Fields, HeadWriter, RequestHead};
use longshore_common::io::{BufId, Completion, Io, Op, Sock, Token};
use longshore_stream::{Step, Stream};
#[cfg(feature = "tls")]
use longshore_tls::handshake::{Handshake, ServerSetup};
use std::io;
use std::net::SocketAddr;

/// An object server over an `Io`, made from a [`ServerConfig`].
pub struct Server {
    cfg: ServerConfig,
    conns: Box<[Conn]>,
    streams: Box<[Stream]>,
    /// Present when the server speaks TLS.
    #[cfg(feature = "tls")]
    tls: Option<ServerSetup>,
    objects: Vec<Object>,
    /// Ids of the multipart uploads in progress; zero is a free entry.
    uploads: [u32; multipart::UPLOADS_MAX],
    uploads_begun: u32,
    /// A batch erase refuses keys that start with this, if it is not empty.
    erase_denied: &'static str,
    /// A listing is refused unless its prefix starts with this.
    list_only_under: &'static str,
    /// The bucket requests name in their path, if the store is addressed
    /// that way. See `serve_bucket_in_path`.
    bucket: &'static str,
    script: [Behavior; SCRIPT_MAX],
    script_head: usize,
    script_len: usize,
    accepting: bool,
    /// Access keys the server accepts; empty means any.
    allowed_keys: [([u8; 64], usize); 2],
    allowed_count: usize,
    /// With a non-zero length, the session token every request must carry.
    session_token: ([u8; 64], usize),
    verify: Option<VerifyFn>,
    /// The last version id given out, if objects are given them.
    versions: Option<u32>,
    /// Objects are encrypted when a request asks for it.
    encrypts: bool,
    /// With `Some`, the SHA-256 header value a stored body must carry.
    sha256: Option<identity::Sha256Fn>,
    /// Connections accepted so far.
    accepted: u64,
    /// Complete requests received so far.
    requests: u64,
    /// Connections closed so far.
    closed: u64,
}

mod behavior;
mod build;
mod conn;
mod consts;
mod describing;
mod events;
mod identity;
mod listing;
mod multipart;
mod object;
mod objects;
mod request;
mod scripted;
mod server_config;
mod server_config_builder;
mod server_config_error;
mod state;
mod transport;
mod xml;

pub use behavior::Behavior;
use conn::Conn;
use consts::{DESCRIBING_MAX, K_CLOSE, K_RECV, K_SEND, LIST_TARGET_MAX, SCRIPT_MAX, TARGET_MAX};
pub use identity::Sha256Fn;
use object::Object;
pub use request::VerifyFn;
pub use server_config::{ChecksumFn, ServerConfig};
pub use server_config_builder::ServerConfigBuilder;
pub use server_config_error::ServerConfigError;
use state::State;

impl Server {
    /// A plain-HTTP server.
    pub fn new(cfg: ServerConfig) -> Server {
        #[cfg(feature = "tls")]
        {
            Self::build(cfg, None)
        }
        #[cfg(not(feature = "tls"))]
        {
            Self::build(cfg)
        }
    }

    /// An HTTPS server. Each connection additionally uses two pool buffers
    /// for sealed records, starting at `first_tls_buf`.
    #[cfg(feature = "tls")]
    pub fn with_tls(cfg: ServerConfig, setup: ServerSetup, first_tls_buf: u32) -> Server {
        Self::build(cfg, Some((setup, first_tls_buf)))
    }

    /// Operations in flight at most: one per connection plus the accept.
    pub fn ops_in_flight_max(cfg: &ServerConfig) -> u32 {
        cfg.connections_max() + 1
    }

    /// Listen and start accepting. Returns the bound address.
    pub fn start<IO: Io>(&mut self, io: &mut IO, addr: SocketAddr) -> io::Result<SocketAddr> {
        let bound = io.listen(self.cfg.listener(), addr)?;
        self.accept(io);
        Ok(bound)
    }

    /// Act as a store whose bucket is addressed in the path: a request for
    /// `/bucket/key` is served as `/key`, and one for `/bucket/?query` as
    /// `/?query`. A request that does not name the bucket is left as it is,
    /// so it finds nothing.
    pub fn serve_bucket_in_path(&mut self, bucket: &'static str) {
        self.bucket = bucket;
    }

    /// Refuse a listing whose prefix does not start with `prefix`, as a
    /// policy that permits listing only part of a bucket does.
    pub fn allow_listing_only_under(&mut self, prefix: &'static str) {
        self.list_only_under = prefix;
    }

    /// Queue a behavior for an upcoming request; unscripted requests are `Normal`.
    pub fn script(&mut self, b: Behavior) {
        assert!(self.script_len < SCRIPT_MAX, "script queue full");
        self.script[(self.script_head + self.script_len) % SCRIPT_MAX] = b;
        self.script_len += 1;
    }

    /// Connections the server currently holds open.
    pub fn open_connections(&self) -> usize {
        self.conns
            .iter()
            .filter(|c| !matches!(c.state, State::Free | State::Accepting | State::Closing))
            .count()
    }

    /// Accept only requests whose `Authorization` header names one of these
    /// access keys (at most two); others get 403. An empty list accepts all.
    /// Signatures are not verified: this models which keys are valid, for
    /// testing credential rotation.
    pub fn allow_access_keys(&mut self, keys: &[&str]) {
        assert!(keys.len() <= 2 && keys.iter().all(|k| k.len() <= 64));
        self.allowed_count = keys.len();
        for (slot, key) in self.allowed_keys.iter_mut().zip(keys) {
            slot.0[..key.len()].copy_from_slice(key.as_bytes());
            slot.1 = key.len();
        }
    }

    /// Refuse with 403 every request `verify` does not accept, the way a
    /// store refuses a request whose signature does not match what it
    /// received. The server cannot check a signature itself, since it does
    /// not know the secret; a test that does supplies the check.
    pub fn verify_requests(&mut self, verify: VerifyFn) {
        self.verify = Some(verify);
    }

    /// Require `x-amz-security-token` to carry exactly this value, as a
    /// store does for temporary keys. An empty value lifts the requirement.
    pub fn require_session_token(&mut self, token: &str) {
        assert!(token.len() <= 64);
        self.session_token.0[..token.len()].copy_from_slice(token.as_bytes());
        self.session_token.1 = token.len();
    }

    /// Connections accepted so far.
    pub fn accepted(&self) -> u64 {
        self.accepted
    }

    /// Complete requests received so far.
    pub fn requests(&self) -> u64 {
        self.requests
    }

    /// Connections closed so far.
    pub fn closed(&self) -> u64 {
        self.closed
    }

    /// Stored size of an object, for assertions.
    pub fn object_len(&self, target: &str) -> Option<u32> {
        self.objects
            .iter()
            .find(|o| o.used && &o.target[..o.target_len] == target.as_bytes())
            .map(|o| o.len)
    }

    /// Make a batch erase refuse every key that starts with `prefix`.
    pub fn deny_erase(&mut self, prefix: &'static str) {
        self.erase_denied = prefix;
    }

    /// Stored bytes of an object, for assertions.
    pub fn object(&self, target: &str) -> Option<&[u8]> {
        self.objects
            .iter()
            .find(|o| o.used && &o.target[..o.target_len] == target.as_bytes())
            .map(|o| &o.data[..o.len as usize])
    }

    fn sock(&self, i: usize) -> Sock {
        Sock(self.cfg.first_sock() + i as u32)
    }

    fn head_buf(&self, i: usize) -> BufId {
        BufId(self.cfg.first_buf() + i as u32 * 2)
    }

    fn body_buf(&self, i: usize) -> BufId {
        BufId(self.cfg.first_buf() + i as u32 * 2 + 1)
    }

    fn accept_token(&self) -> u64 {
        self.cfg.token_base() + u64::from(self.cfg.connections_max()) * 8
    }

    fn submit<IO: Io>(&self, io: &mut IO, i: usize, kind: u64, op: Op) {
        Self::submit_as(io, Token(self.cfg.token_base() + i as u64 * 8 + kind), op);
    }

    #[expect(
        clippy::expect_used,
        reason = "a server keeps at most ops_in_flight_max operations, which Limits::ops_max must cover"
    )]
    fn submit_as<IO: Io>(io: &mut IO, token: Token, op: Op) {
        io.submit(token, op)
            .expect("Limits::ops_max must cover Server::ops_in_flight_max");
    }
}

/// Decode `%XX` sequences into `out`; malformed sequences are kept as written.
/// Returns the decoded length, at most `out.len()`.
fn percent_decode(input: &[u8], out: &mut [u8]) -> usize {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let (mut i, mut n) = (0, 0);
    while i < input.len() && n < out.len() {
        let decoded = (input[i] == b'%')
            .then(|| Some(hex(*input.get(i + 1)?)? << 4 | hex(*input.get(i + 2)?)?))
            .flatten();
        match decoded {
            Some(b) => {
                out[n] = b;
                i += 3;
            }
            None => {
                out[n] = input[i];
                i += 1;
            }
        }
        n += 1;
    }
    n
}

/// Uppercase hex digits of `v`, right-aligned in 16 bytes, and how many there are.
fn format_hex(mut v: usize) -> ([u8; 16], usize) {
    let mut out = [b'0'; 16];
    let mut len = 0;
    loop {
        len += 1;
        out[16 - len] = b"0123456789ABCDEF"[v & 15];
        v >>= 4;
        if v == 0 {
            return (out, len);
        }
    }
}
