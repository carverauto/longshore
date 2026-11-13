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

use crate::http::head::{parse_request, HeadWriter};
use crate::http::stream::{Step, Stream};
use crate::io::{BufId, Completion, Io, Op, Sock, Token};
use crate::tls::handshake::{Handshake, ServerSetup};
use std::io;
use std::net::SocketAddr;

/// Maps a body to the checksum header value it must carry.
///
/// Two forms are understood. Twelve base64 characters are a CRC-64/NVME as S3
/// carries it in `x-amz-checksum-crc64nvme`. Eight base64 characters followed
/// by four zero bytes are a CRC-32C as Google Cloud Storage carries it in
/// `x-goog-hash: crc32c=...`.
pub type ChecksumFn = fn(&[u8]) -> [u8; 12];

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub listener: Sock,
    pub connections_max: u32,
    /// Socket slots `first_sock..first_sock + connections_max`.
    pub first_sock: u32,
    /// Two buffers per connection, head then body, starting here.
    pub first_buf: u32,
    pub head_max: u32,
    pub buffer_size: u32,
    /// Objects the store can hold. Each reserves `buffer_size` bytes at init.
    pub objects_max: u32,
    /// Tokens used are `token_base..token_base + connections_max * 8 + 1`.
    pub token_base: u64,
    /// If set, the value an `x-amz-checksum-crc64nvme` header must have for
    /// a given body. A PUT whose header disagrees is refused with a
    /// `BadDigest` error document, and GET returns the stored value.
    pub checksum: Option<ChecksumFn>,
}

/// How to treat the next request. Queued with [`Server::script`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behavior {
    Normal,
    /// Respond with this status and an empty body.
    Status(u16),
    /// Respond with this status and a body of this many filler bytes, as
    /// real stores do with their error documents.
    StatusWithBody(u16, u32),
    /// Close the connection without responding.
    CloseBeforeResponse,
    /// Over TLS: answer with a fatal alert of this description, then close.
    TlsAlert(u8),
    /// Carry out the request, then close without responding: the response is lost.
    ApplyThenClose,
    /// Send the head and half the body, then close.
    TruncateBody,
    /// Read the request and never respond.
    Stall,
    /// Respond with bytes that are not HTTP.
    GarbageHead,
    /// Respond with a head that never ends.
    EndlessHead,
    /// Send exactly these bytes as the whole response, then close.
    Raw(&'static [u8]),
    /// Respond with this status and an error document naming this code.
    /// Real stores can do this with status 200.
    ErrorDocument(u16, &'static str),
    /// Respond with chunked transfer encoding and an empty body.
    Chunked,
    /// Serve a GET of a stored object with chunked transfer encoding, in
    /// chunks of this many bytes, on a connection that stays open.
    ChunkedBody(u32),
    /// Respond with chunked transfer encoding whose first chunk size is not a number.
    BadChunk,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Free,
    /// Reserved for the accept in flight.
    Accepting,
    /// Accepted; the TLS handshake is in progress.
    Handshake,
    RecvHead {
        have: u32,
    },
    RecvBody {
        have: u32,
    },
    SendHead {
        off: u32,
    },
    SendBody {
        off: u32,
    },
    Stalled,
    Closing,
}

const TARGET_MAX: usize = 256;
/// A listing's request target: one that fitted before, and a continuation
/// token for a cursor of up to `TARGET_MAX` bytes.
const LIST_TARGET_MAX: usize = TARGET_MAX + b"continuation-token=t&".len() + 2 * TARGET_MAX;
/// Room for the headers that describe an object: content type and metadata.
const DESCRIBING_MAX: usize = 192;

#[derive(Clone, Copy)]
struct Conn {
    state: State,
    target: [u8; LIST_TARGET_MAX],
    target_len: usize,
    method: [u8; 8],
    method_len: usize,
    request_len: u32,
    head_len: u32,
    body_len: u32,
    close_after: bool,
    /// `If-None-Match: *` was sent: store only if the object does not exist.
    if_none_match: bool,
    /// `Range: bytes=a-b`, inclusive.
    range: Option<(u32, u32)>,
    /// Checksum header value sent with the request.
    claimed: Option<[u8; 12]>,
    /// Checksum header value to send with the response.
    reply_checksum: Option<[u8; 12]>,
    /// The request carried `x-amz-checksum-mode: ENABLED`.
    checksum_mode: bool,
    /// The request's access key is on the allowed list (or no list is set).
    authorized: bool,
    /// The request carried a bearer token: answer in Google Cloud Storage's dialect.
    bearer: bool,
    /// `ETag` to send with the response: a stored part's tag.
    reply_tag: Option<[u8; multipart::TAG_LEN]>,
    /// The request's content type and metadata headers, as raw lines.
    describing: [u8; DESCRIBING_MAX],
    describing_len: usize,
    /// The request's copy source, `/bucket/key`, if it is a copy.
    copy_source: [u8; TARGET_MAX],
    copy_source_len: usize,
    /// The object whose tag, date and describing headers go on the response.
    reply_object: Option<u32>,
}

struct Object {
    target: [u8; TARGET_MAX],
    target_len: usize,
    len: u32,
    data: Box<[u8]>,
    used: bool,
    checksum: Option<[u8; 12]>,
    /// Content type and metadata headers it was stored with, as raw lines.
    describing: [u8; DESCRIBING_MAX],
    describing_len: usize,
}

const K_RECV: u64 = 0;
const K_SEND: u64 = 1;
const K_CLOSE: u64 = 2;
const SCRIPT_MAX: usize = 32;

pub struct Server {
    cfg: Config,
    conns: Box<[Conn]>,
    streams: Box<[Stream]>,
    /// Present when the server speaks TLS.
    tls: Option<ServerSetup>,
    objects: Vec<Object>,
    /// Ids of the multipart uploads in progress; zero is a free entry.
    uploads: [u32; multipart::UPLOADS_MAX],
    uploads_begun: u32,
    /// A batch erase refuses keys that start with this, if it is not empty.
    erase_denied: &'static str,
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
    /// Connections accepted so far.
    pub accepted: u64,
    /// Complete requests received so far.
    pub requests: u64,
    /// Connections closed so far.
    pub closed: u64,
}

mod describing;
mod events;
mod listing;
mod multipart;
mod objects;
mod transport;

impl Server {
    /// A plain-HTTP server.
    pub fn new(cfg: Config) -> Server {
        Self::build(cfg, None)
    }

    /// An HTTPS server. Each connection additionally uses two pool buffers
    /// for sealed records, starting at `first_tls_buf`.
    pub fn with_tls(cfg: Config, setup: ServerSetup, first_tls_buf: u32) -> Server {
        Self::build(cfg, Some((setup, first_tls_buf)))
    }

    fn build(cfg: Config, tls: Option<(ServerSetup, u32)>) -> Server {
        assert!(cfg.head_max <= cfg.buffer_size && cfg.head_max <= u16::MAX as u32);
        let conn = Conn {
            state: State::Free,
            target: [0; LIST_TARGET_MAX],
            target_len: 0,
            method: [0; 8],
            method_len: 0,
            request_len: 0,
            head_len: 0,
            body_len: 0,
            close_after: false,
            if_none_match: false,
            range: None,
            claimed: None,
            reply_checksum: None,
            checksum_mode: false,
            authorized: true,
            bearer: false,
            reply_tag: None,
            describing: [0; DESCRIBING_MAX],
            describing_len: 0,
            copy_source: [0; TARGET_MAX],
            copy_source_len: 0,
            reply_object: None,
        };
        let objects = (0..cfg.objects_max)
            .map(|_| Object {
                target: [0; TARGET_MAX],
                target_len: 0,
                len: 0,
                data: vec![0u8; cfg.buffer_size as usize].into_boxed_slice(),
                used: false,
                checksum: None,
                describing: [0; DESCRIBING_MAX],
                describing_len: 0,
            })
            .collect();
        let first_tls_buf = tls.as_ref().map(|t| t.1);
        let streams = (0..cfg.connections_max)
            .map(|i| {
                let token = |kind| Token(cfg.token_base + i as u64 * 8 + kind);
                let s = Stream::new(Sock(cfg.first_sock + i), token(K_SEND), token(K_RECV));
                match first_tls_buf {
                    Some(first) => s.with_tls_buffers(
                        BufId(first + 2 * i),
                        BufId(first + 2 * i + 1),
                        cfg.buffer_size,
                        cfg.buffer_size,
                        u32::MAX,
                    ),
                    None => s,
                }
            })
            .collect();
        Server {
            cfg,
            conns: vec![conn; cfg.connections_max as usize].into_boxed_slice(),
            streams,
            tls: tls.map(|t| t.0),
            objects,
            uploads: [0; multipart::UPLOADS_MAX],
            uploads_begun: 0,
            erase_denied: "",
            bucket: "",
            script: [Behavior::Normal; SCRIPT_MAX],
            script_head: 0,
            script_len: 0,
            accepting: false,
            allowed_keys: [([0; 64], 0); 2],
            allowed_count: 0,
            session_token: ([0; 64], 0),
            accepted: 0,
            requests: 0,
            closed: 0,
        }
    }

    /// Operations in flight at most: one per connection plus the accept.
    pub fn ops_in_flight_max(cfg: &Config) -> u32 {
        cfg.connections_max + 1
    }

    /// Listen and start accepting. Returns the bound address.
    pub fn start<IO: Io>(&mut self, io: &mut IO, addr: SocketAddr) -> io::Result<SocketAddr> {
        let bound = io.listen(self.cfg.listener, addr)?;
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

    /// Require `x-amz-security-token` to carry exactly this value, as a
    /// store does for temporary keys. An empty value lifts the requirement.
    pub fn require_session_token(&mut self, token: &str) {
        assert!(token.len() <= 64);
        self.session_token.0[..token.len()].copy_from_slice(token.as_bytes());
        self.session_token.1 = token.len();
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
        Sock(self.cfg.first_sock + i as u32)
    }

    fn head_buf(&self, i: usize) -> BufId {
        BufId(self.cfg.first_buf + i as u32 * 2)
    }

    fn body_buf(&self, i: usize) -> BufId {
        BufId(self.cfg.first_buf + i as u32 * 2 + 1)
    }

    fn accept_token(&self) -> u64 {
        self.cfg.token_base + self.cfg.connections_max as u64 * 8
    }

    fn submit<IO: Io>(&self, io: &mut IO, i: usize, kind: u64, op: Op) {
        io.submit(Token(self.cfg.token_base + i as u64 * 8 + kind), op)
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
