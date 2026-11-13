// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP/1.1 client over `Io`: a fixed pool of connections, one request per
//! connection at a time.
//!
//! The request head is written into the connection's own buffer. The request
//! body is sent straight from the caller's buffer and the response body is
//! received straight into the caller's buffer; neither is copied by the
//! client, apart from the few response-body bytes that arrive in the same
//! read as the head.
//!
//! The owner routes `Io` completions to [`Client::on_completion`] and calls
//! [`Client::tick`] at its own pace to enforce request timeouts.

use crate::chunked::Chunked;
use crate::head::{parse_response, Fields, HeadWriter, HttpError};
use crate::stream::{IdleCheck, Step, Stream};
use osc_io::{BufId, Completion, Io, Op, Sock, Token, ECANCELED};
use osc_tls::handshake::{ClientSetup, Handshake, TlsError};
use std::net::SocketAddrV4;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub connections_max: u32,
    /// Socket slots `first_sock..first_sock + connections_max`.
    pub first_sock: u32,
    /// One head buffer per connection: `first_buf..first_buf + connections_max`.
    pub first_buf: u32,
    /// Largest request or response head, at most the pool buffer size and 65535.
    pub head_max: u32,
    /// Size of a pool buffer; response bodies must fit in one.
    pub buffer_size: u32,
    /// An idle connection older than this is replaced before reuse.
    pub max_age_ns: u64,
    /// A connection is replaced after this many requests.
    pub max_requests: u32,
    /// A request taking longer than this is cancelled and fails.
    pub request_timeout_ns: u64,
    /// Request bodies at least this large are sent zero-copy. The kernel
    /// copies small zero-copy sends anyway, so this should be large.
    pub zero_copy_min: u32,
    /// Tokens used are `token_base..token_base + connections_max * 8`.
    pub token_base: u64,
    /// With TLS, the most ciphertext sealed before each send. See
    /// `Stream::with_tls_buffers`.
    pub tls_batch: u32,
    /// With TLS, a connection that has protected this many records in either
    /// direction is closed after its current request, so traffic keys are
    /// replaced long before they wear out. At most, and normally exactly,
    /// [`TLS_RECORDS_MAX`].
    pub tls_records_max: u64,
    /// Before an idle connection carries another request, look at it without
    /// waiting. Bytes the last response did not declare, or a connection the
    /// peer has closed, mean the request goes out on a new connection.
    /// Costs one extra operation per reused connection.
    pub check_idle: bool,
}

/// The default and largest `tls_records_max`.
pub const TLS_RECORDS_MAX: u64 = osc_tls::record::KEY_RECORDS_RETIRE;

pub struct Request<'a> {
    pub method: &'a str,
    /// Origin-form target, already percent-encoded: `/bucket/key?query`.
    pub target: &'a str,
    /// Value of the Host header.
    pub host: &'a str,
    /// Connections are reused only for the same `host_id`.
    pub host_id: u16,
    /// Address to connect to if a new connection is needed.
    pub addr: SocketAddrV4,
    pub headers: &'a [(&'a str, &'a [u8])],
    /// Request body: a pool buffer and the number of bytes to send from it.
    pub body: Option<(BufId, u32)>,
    /// Where to receive the response body. Required if the response has one.
    pub response_buf: Option<BufId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Length of the response body. If no response buffer was given the
    /// body was read and discarded.
    pub body_len: u32,
    /// Connection that served it; pass to [`Client::header`].
    pub conn: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Every connection is in use.
    Busy,
    /// The request head is invalid or does not fit.
    BadRequest,
    /// The response head is malformed or too large.
    Head(HttpError),
    /// A socket operation failed with this errno.
    Io(i32),
    /// The peer closed mid-exchange. `reused` is true when the connection had
    /// served earlier requests, the usual sign of a stale keep-alive.
    Closed {
        reused: bool,
    },
    Timeout,
    /// The response body does not fit the buffer, or it is too large to
    /// discard when no buffer was given.
    BodyTooLarge,
    /// A response body with neither a length nor chunked encoding (it would
    /// end only when the connection closes) is not supported.
    UnsupportedFraming,
    /// The TLS handshake failed or a record did not authenticate.
    Tls(TlsError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done {
    pub token: u64,
    pub result: Result<Response, Error>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// The completion does not belong to this client.
    NotMine,
    /// Consumed; the request is still in progress.
    Pending,
    Done(Done),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Closed,
    /// Closing an old connection; a loaded request connects afterwards.
    Closing {
        then_connect: bool,
    },
    Connecting,
    /// An idle connection is being looked at before it is used again; the
    /// request's head is already in the head buffer.
    CheckIdle,
    /// TCP is up; the TLS handshake is in progress.
    TlsHandshake,
    SendHead {
        off: u32,
    },
    SendBody {
        off: u32,
    },
    RecvHead {
        have: u32,
    },
    RecvBody {
        have: u32,
    },
    /// Reading and discarding a body the caller gave no buffer for (an
    /// error document, typically), through the spare room in the head buffer.
    Drain {
        have: u32,
    },
    /// Receiving a chunked body; `decoded` is the offset in the body's
    /// buffer where decoded bytes end and newly received bytes go.
    RecvChunked {
        decoded: u32,
    },
    /// The chunked body filled the caller's buffer exactly; only the end of
    /// the framing is still to come, read into the space after the head.
    RecvChunkedTail,
    Idle,
}

#[derive(Clone, Copy)]
struct Conn {
    state: State,
    host_id: u16,
    addr: SocketAddrV4,
    opened_at: u64,
    uses: u32,
    // Current request.
    token: u64,
    started_at: u64,
    head_len: u32,
    body: Option<(BufId, u32)>,
    response_buf: Option<BufId>,
    is_head: bool,
    timed_out: bool,
    status: u16,
    body_len: u32,
    fields: Option<Fields>,
    /// Chunked decoding state, and where the body starts in its buffer.
    chunked: Chunked,
    body_base: u32,
    /// Server name for TLS, copied from the request's host.
    name: [u8; 253],
    name_len: u8,
}

/// Largest body the client will read and discard for a caller with no buffer.
const DRAIN_MAX: u64 = 256 * 1024;

const K_CONNECT: u64 = 0;
const K_SEND: u64 = 1;
const K_RECV: u64 = 2;
const K_CLOSE: u64 = 3;
const K_CANCEL: u64 = 4;

pub struct Client {
    cfg: Config,
    conns: Box<[Conn]>,
    streams: Box<[Stream]>,
    /// Present when connections use TLS.
    tls: Option<ClientSetup>,
    /// Connections opened so far.
    pub connects: u64,
    /// Idle connections found closed by the peer and replaced before use.
    pub idle_closed: u64,
    /// Idle connections found holding bytes nobody asked for, and replaced.
    pub idle_dirty: u64,
}

mod chunked_body;
mod events;
mod request;
mod response;

impl Client {
    /// A client that speaks plain HTTP.
    pub fn new(cfg: Config) -> Client {
        Self::build(cfg, None)
    }

    /// A client that speaks HTTPS. Each connection additionally uses two
    /// pool buffers, for sealed records going out and coming in, starting at
    /// `first_tls_buf`: `first_tls_buf..first_tls_buf + 2 * connections_max`.
    pub fn with_tls(cfg: Config, setup: ClientSetup, first_tls_buf: u32) -> Client {
        Self::build(cfg, Some((setup, first_tls_buf)))
    }

    fn build(cfg: Config, tls: Option<(ClientSetup, u32)>) -> Client {
        assert!(
            cfg.head_max <= cfg.buffer_size && cfg.head_max <= u16::MAX as u32,
            "head_max must fit a pool buffer and 16 bits"
        );
        assert!(
            cfg.tls_records_max <= TLS_RECORDS_MAX,
            "tls_records_max must not exceed TLS_RECORDS_MAX"
        );
        let conn = Conn {
            state: State::Closed,
            host_id: 0,
            addr: SocketAddrV4::new(0.into(), 0),
            opened_at: 0,
            uses: 0,
            token: 0,
            started_at: 0,
            head_len: 0,
            body: None,
            response_buf: None,
            is_head: false,
            timed_out: false,
            status: 0,
            body_len: 0,
            fields: None,
            chunked: Chunked::new(),
            body_base: 0,
            name: [0; 253],
            name_len: 0,
        };
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
                        cfg.tls_batch,
                        cfg.zero_copy_min,
                    ),
                    None => s,
                }
            })
            .collect();
        Client {
            cfg,
            conns: vec![conn; cfg.connections_max as usize].into_boxed_slice(),
            streams,
            tls: tls.map(|t| t.0),
            connects: 0,
            idle_closed: 0,
            idle_dirty: 0,
        }
    }

    /// Operations the client can have in flight: one per connection plus up
    /// to two cancels for each (a TLS handshake may have either a send or a
    /// receive outstanding).
    pub fn ops_in_flight_max(cfg: &Config) -> u32 {
        cfg.connections_max * 3
    }

    fn sock(&self, i: usize) -> Sock {
        Sock(self.cfg.first_sock + i as u32)
    }

    fn head_buf(&self, i: usize) -> BufId {
        BufId(self.cfg.first_buf + i as u32)
    }

    fn token(&self, i: usize, kind: u64) -> Token {
        Token(self.cfg.token_base + i as u64 * 8 + kind)
    }

    fn submit<IO: Io>(&self, io: &mut IO, i: usize, kind: u64, op: Op) {
        io.submit(self.token(i, kind), op)
            .expect("Limits::ops_max must cover Client::ops_in_flight_max");
    }

    /// Connections currently open or opening, idle ones included.
    pub fn open_connections(&self) -> usize {
        self.conns
            .iter()
            .filter(|c| {
                !matches!(
                    c.state,
                    State::Closed
                        | State::Closing {
                            then_connect: false
                        }
                )
            })
            .count()
    }

    /// Requests currently in progress.
    pub fn in_flight(&self) -> usize {
        self.conns
            .iter()
            .filter(|c| {
                !matches!(
                    c.state,
                    State::Closed
                        | State::Idle
                        | State::Closing {
                            then_connect: false
                        }
                )
            })
            .count()
    }

    /// A header of the last response served by `conn`. Valid until that
    /// connection starts another request.
    pub fn header<'a, IO: Io>(&self, io: &'a mut IO, conn: u32, name: &str) -> Option<&'a [u8]> {
        let fields = self.conns[conn as usize].fields?;
        fields.header(io.buf(self.head_buf(conn as usize)), name)
    }

    /// The body of the last response served by `conn`, if the caller gave no
    /// buffer and it was small enough to keep: error documents, typically.
    /// Empty if the body did not fit beside the head. Valid until that
    /// connection starts another request.
    pub fn discarded_body<'a, IO: Io>(&self, io: &'a mut IO, conn: u32) -> &'a [u8] {
        let c = &self.conns[conn as usize];
        let room = self.cfg.head_max - c.head_len;
        if c.fields.is_none() || c.response_buf.is_some() || c.body_len > room {
            return &[];
        }
        let start = c.head_len as usize;
        &io.buf(self.head_buf(conn as usize))[start..start + c.body_len as usize]
    }
}
