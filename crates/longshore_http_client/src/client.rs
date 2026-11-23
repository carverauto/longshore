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
use crate::head::{parse_response, HeadWriter, HttpError, ResponseHead};
#[cfg(feature = "tls")]
use longshore_common::io::ECANCELED;
use longshore_common::io::{BufId, Completion, Io, Op, Segment, Sock, Token};
use longshore_stream::{IdleCheck, Step, Stream};
#[cfg(feature = "tls")]
use longshore_tls::handshake::{ClientSetup, Handshake};
use std::net::SocketAddr;

pub struct Client {
    cfg: ClientConfig,
    conns: Box<[Conn]>,
    streams: Box<[Stream]>,
    /// Present when connections use TLS.
    #[cfg(feature = "tls")]
    tls: Option<ClientSetup>,
    /// Connections opened so far.
    connects: u64,
    /// Idle connections found closed by the peer and replaced before use.
    idle_closed: u64,
    /// Idle connections found holding bytes nobody asked for, and replaced.
    idle_dirty: u64,
    /// The address of the last connection that failed or timed out before it
    /// opened. The owner takes it to learn that an address is unreachable.
    connect_failure: Option<SocketAddr>,
    /// The address and segment sizes of the last connection opened, when
    /// the I/O backend measured them. The owner takes it.
    connected: Option<(SocketAddr, Segment)>,
}

mod chunked_body;
mod client_config;
mod client_config_builder;
mod client_config_error;
mod conn;
mod consts;
mod done;
mod error;
mod events;
mod handled;
mod plain;
mod receive;
mod request;
mod response;
mod retire;
mod start;
mod state;

pub use client_config::ClientConfig;
pub use client_config_builder::{
    ClientConfigBuilder, HEAD_MAX_DEFAULT, MAX_AGE_NS_DEFAULT, MAX_REQUESTS_DEFAULT,
    REQUEST_TIMEOUT_NS_DEFAULT, TLS_BATCH_DEFAULT_MAX, ZERO_COPY_MIN_DEFAULT,
};
pub use client_config_error::ClientConfigError;
use conn::Conn;
pub use consts::TLS_RECORDS_MAX;
use consts::{DRAIN_MAX, K_CANCEL, K_CLOSE, K_CONNECT, K_RECV, K_SEND};
pub use done::Done;
pub use error::Error;
pub use handled::Handled;
pub use request::Request;
pub use response::Response;
use state::State;

impl Client {
    /// A client that speaks plain HTTP.
    pub fn new(cfg: ClientConfig) -> Client {
        #[cfg(feature = "tls")]
        {
            Self::build(cfg, None)
        }
        #[cfg(not(feature = "tls"))]
        {
            Self::build(cfg)
        }
    }

    /// A client that speaks HTTPS. Each connection additionally uses two
    /// pool buffers, for sealed records going out and coming in, starting at
    /// `first_tls_buf`: `first_tls_buf..first_tls_buf + 2 * connections_max`.
    #[cfg(feature = "tls")]
    pub fn with_tls(cfg: ClientConfig, setup: ClientSetup, first_tls_buf: u32) -> Client {
        Self::build(cfg, Some((setup, first_tls_buf)))
    }

    #[cfg(feature = "tls")]
    fn build(cfg: ClientConfig, tls: Option<(ClientSetup, u32)>) -> Client {
        Self::build_inner(cfg, Some(tls))
    }

    #[cfg(not(feature = "tls"))]
    fn build(cfg: ClientConfig) -> Client {
        Self::build_inner(cfg)
    }

    fn build_inner(
        cfg: ClientConfig,
        #[cfg(feature = "tls")] tls: Option<Option<(ClientSetup, u32)>>,
    ) -> Client {
        // `ClientConfig::build` has checked the slot, token and head limits.
        let conn = Conn::new();
        #[cfg(feature = "tls")]
        let tls = tls.flatten();
        #[cfg(feature = "tls")]
        let first_tls_buf = tls.as_ref().map(|t| t.1);
        let streams = (0..cfg.connections_max())
            .map(|i| {
                let token = |kind| Token(cfg.token_base() + u64::from(i) * 8 + kind);
                let s = Stream::new(Sock(cfg.first_sock() + i), token(K_SEND), token(K_RECV));
                #[cfg(feature = "tls")]
                let s = match first_tls_buf {
                    Some(first) => s.with_tls_buffers(
                        BufId(first + 2 * i),
                        BufId(first + 2 * i + 1),
                        cfg.buffer_size(),
                        cfg.tls_batch(),
                        cfg.zero_copy_min(),
                    ),
                    None => s,
                };
                s
            })
            .collect();
        Client {
            cfg,
            conns: vec![conn; cfg.connections_max() as usize].into_boxed_slice(),
            streams,
            #[cfg(feature = "tls")]
            tls: tls.map(|t| t.0),
            connects: 0,
            idle_closed: 0,
            idle_dirty: 0,
            connect_failure: None,
            connected: None,
        }
    }

    /// Operations the client can have in flight: one per connection plus up
    /// to two cancels for each (a TLS handshake may have either a send or a
    /// receive outstanding).
    pub fn ops_in_flight_max(cfg: &ClientConfig) -> u32 {
        cfg.connections_max() * 3
    }

    fn sock(&self, i: usize) -> Sock {
        Sock(self.cfg.first_sock() + i as u32)
    }

    fn head_buf(&self, i: usize) -> BufId {
        BufId(self.cfg.first_buf() + i as u32)
    }

    fn token(&self, i: usize, kind: u64) -> Token {
        Token(self.cfg.token_base() + i as u64 * 8 + kind)
    }

    #[expect(
        clippy::expect_used,
        reason = "a client keeps at most ops_in_flight_max operations, which Limits::ops_max must cover"
    )]
    fn submit<IO: Io>(&self, io: &mut IO, i: usize, kind: u64, op: Op) {
        io.submit(self.token(i, kind), op)
            .expect("Limits::ops_max must cover Client::ops_in_flight_max");
    }

    /// Connections opened so far.
    #[inline]
    #[must_use]
    pub const fn connects(&self) -> u64 {
        self.connects
    }

    /// Idle connections found closed by the peer and replaced before use.
    #[inline]
    #[must_use]
    pub const fn idle_closed(&self) -> u64 {
        self.idle_closed
    }

    /// Idle connections found holding bytes nobody asked for, and replaced.
    #[inline]
    #[must_use]
    pub const fn idle_dirty(&self) -> u64 {
        self.idle_dirty
    }

    /// The address of the last connection that failed or timed out before
    /// it opened, if it has not been taken.
    #[inline]
    #[must_use]
    pub const fn connect_failure(&self) -> Option<SocketAddr> {
        self.connect_failure
    }

    /// Take the address of the last connection that failed or timed out
    /// before it opened, to learn that an address is unreachable.
    #[inline]
    pub const fn take_connect_failure(&mut self) -> Option<SocketAddr> {
        self.connect_failure.take()
    }

    /// The address and segment sizes of the last connection opened, when the
    /// I/O backend measured them, if they have not been taken.
    #[inline]
    #[must_use]
    pub const fn connected(&self) -> Option<(SocketAddr, Segment)> {
        self.connected
    }

    /// Take the address and segment sizes of the last connection opened.
    #[inline]
    pub const fn take_connected(&mut self) -> Option<(SocketAddr, Segment)> {
        self.connected.take()
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

    /// Every value of a header of the last response served by `conn`, one
    /// for each line it was sent on. Valid until that connection starts
    /// another request.
    pub fn headers<'a, IO: Io>(
        &self,
        io: &'a mut IO,
        conn: u32,
        name: &'a str,
    ) -> impl Iterator<Item = &'a [u8]> {
        let fields = self.conns[conn as usize].fields;
        let buf: &'a [u8] = io.buf(self.head_buf(conn as usize));
        fields.into_iter().flat_map(move |f| f.headers(buf, name))
    }

    /// The `Content-Length` of the last response served by `conn`. For a
    /// HEAD this is the length of the body that was not sent.
    pub fn content_length(&self, conn: u32) -> Option<u64> {
        self.conns[conn as usize].fields?.content_length()
    }

    /// Copy the head of the last response served by `conn` into `into`, as
    /// much as fits, and return how many bytes that was.
    pub fn copy_head<IO: Io>(&self, io: &mut IO, conn: u32, into: BufId) -> u32 {
        let c = &self.conns[conn as usize];
        if c.fields.is_none() {
            return 0;
        }
        let (head, out) = io.buf_pair(self.head_buf(conn as usize), into);
        let n = (c.head_len as usize).min(out.len());
        out[..n].copy_from_slice(&head[..n]);
        n as u32
    }

    /// The body of the last response served by `conn`, if the caller gave no
    /// buffer and it was small enough to keep: error documents, typically.
    /// Empty if the body did not fit beside the head. Valid until that
    /// connection starts another request.
    pub fn discarded_body<'a, IO: Io>(&self, io: &'a mut IO, conn: u32) -> &'a [u8] {
        let c = &self.conns[conn as usize];
        let room = self.cfg.head_max() - c.head_len;
        if c.fields.is_none() || c.response_buf.is_some() || c.body_len > room {
            return &[];
        }
        let start = c.head_len as usize;
        &io.buf(self.head_buf(conn as usize))[start..start + c.body_len as usize]
    }
}
