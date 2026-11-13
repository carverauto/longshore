// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One connection's byte stream over `Io`, plain or TLS.
//!
//! The HTTP client and the test server ask a [`Stream`] to send bytes from a
//! buffer or receive bytes into one. Plain streams pass the operation
//! straight to `Io`. TLS streams seal records from the source buffer into a
//! send buffer, and open received records into the destination buffer; the
//! owner sees plaintext lengths either way.
//!
//! With TLS the only passes over payload bytes are the cipher's own: sealing
//! reads the caller's buffer and writes the send buffer; opening reads the
//! receive buffer and writes the caller's buffer. The exception is a record
//! whose content does not fit the space left in the destination, which is
//! opened where it lies and copied out.

use osc_io::{BufId, Io, Op, Sock, Token};
use osc_tls::handshake::{Handshake, TlsError};
use osc_tls::record::RecordError;
use osc_tls::record::{
    record_len, Direction, CONTENT_ALERT, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE,
    FRAGMENT_MAX, HEADER_LEN, OVERHEAD, TAG_LEN,
};

mod records;

/// Outcome of feeding a completion to a stream.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Step {
    /// The stream submitted more I/O; nothing to report yet.
    Pending,
    /// The operation the owner asked for finished with this result: bytes of
    /// plaintext sent or received, 0 for end of stream, or a negated errno.
    Done(i32),
    Tls(TlsError),
}

struct Tls {
    handshake: Option<Handshake>,
    tx: Direction,
    /// Content type of the records the current send produces.
    tx_type: u8,
    /// Sealing failed part-way through a send: the keys have done all the
    /// work they may. Reported once what was sealed before it has gone out.
    tx_error: Option<RecordError>,
    rx: Direction,
    tx_buf: BufId,
    rx_buf: BufId,
    /// Size of each of the two buffers.
    cap: u32,
    /// Most ciphertext sealed into the send buffer before sending it.
    batch: u32,
    // Send in progress: plaintext source and how far it has been sealed.
    src: BufId,
    src_off: u32,
    src_len: u32,
    sealed: u32,
    /// Bytes in `tx_buf` to send, and how many have gone.
    cipher_len: u32,
    cipher_sent: u32,
    zero_copy_min: u32,
    // Receive: unprocessed ciphertext is `rx_buf[rx_start..rx_end]`.
    rx_start: u32,
    rx_end: u32,
    /// Content opened in place that did not fit the destination.
    spill_at: u32,
    spill_len: u32,
    dst: BufId,
    dst_off: u32,
    dst_max: u32,
    /// The peer sent a close alert.
    closed: bool,
    /// A handshake has been started on the current connection; until then,
    /// and after `reset`, the stream is plain.
    active: bool,
}

pub struct Stream {
    sock: Sock,
    send_token: Token,
    recv_token: Token,
    tls: Option<Tls>,
}

mod handshake;
mod idle;
pub use idle::IdleCheck;

impl Stream {
    /// A plain stream. `send_token` and `recv_token` identify its operations.
    pub fn new(sock: Sock, send_token: Token, recv_token: Token) -> Stream {
        Stream {
            sock,
            send_token,
            recv_token,
            tls: None,
        }
    }

    /// Give the stream TLS buffers. Until [`Stream::start_handshake`] it stays plain.
    /// `cap` is the size of both buffers and must hold at least two records.
    ///
    /// `batch` bounds how much is sealed before each send. Sealing writes the
    /// send buffer, so a batch that fits in the CPU cache is cheaper than one
    /// that does not; it is clamped to at least one record and at most `cap`.
    pub fn with_tls_buffers(
        mut self,
        tx_buf: BufId,
        rx_buf: BufId,
        cap: u32,
        batch: u32,
        zero_copy_min: u32,
    ) -> Stream {
        assert!(
            cap as usize >= 2 * (FRAGMENT_MAX + OVERHEAD + 256),
            "TLS buffers must hold two records"
        );
        let batch = batch.clamp((FRAGMENT_MAX + OVERHEAD) as u32, cap);
        self.tls = Some(Tls {
            handshake: None,
            tx: Direction::new(),
            tx_type: CONTENT_APPLICATION_DATA,
            tx_error: None,
            rx: Direction::new(),
            tx_buf,
            rx_buf,
            cap,
            batch,
            src: BufId(0),
            src_off: 0,
            src_len: 0,
            sealed: 0,
            cipher_len: 0,
            cipher_sent: 0,
            zero_copy_min,
            rx_start: 0,
            rx_end: 0,
            spill_at: 0,
            spill_len: 0,
            dst: BufId(0),
            dst_off: 0,
            dst_max: 0,
            closed: false,
            active: false,
        });
        self
    }

    /// True if this stream is protected (its handshake has been started).
    pub fn is_tls(&self) -> bool {
        self.tls.as_ref().is_some_and(|t| t.active)
    }

    /// Decrypted application data is waiting that the reader did not ask for.
    /// After a complete response that means the peer sent more than it declared.
    pub fn has_unread_plaintext(&self) -> bool {
        self.tls
            .as_ref()
            .is_some_and(|t| t.active && t.spill_len > 0)
    }

    /// Records protected so far with the current keys, in the busier direction.
    /// Zero without TLS.
    pub fn records(&self) -> u64 {
        self.tls
            .as_ref()
            .filter(|t| t.active)
            .map_or(0, |t| t.tx.sequence().max(t.rx.sequence()))
    }

    /// Forget keys and buffered bytes; the stream is plain again until the
    /// next handshake. Call when the connection closes.
    pub fn reset(&mut self) {
        if let Some(t) = &mut self.tls {
            t.handshake = None;
            t.tx.clear();
            t.tx_error = None;
            t.rx.clear();
            (
                t.rx_start,
                t.rx_end,
                t.spill_len,
                t.cipher_len,
                t.cipher_sent,
                t.closed,
                t.active,
            ) = (0, 0, 0, 0, 0, false, false);
        }
    }

    fn submit<IO: Io>(io: &mut IO, token: Token, op: Op) {
        io.submit(token, op)
            .expect("Limits::ops_max must cover one send and one receive per connection");
    }

    // ---- send ------------------------------------------------------------

    /// Send `len` bytes of `buf` from `off`. On a plain stream this may
    /// complete short, exactly as `Io` does; on a TLS stream it completes
    /// only when everything has been sent.
    pub fn send<IO: Io>(
        &mut self,
        io: &mut IO,
        buf: BufId,
        off: u32,
        len: u32,
        zero_copy_min: u32,
    ) {
        let (sock, send_token) = (self.sock, self.send_token);
        match self.tls.as_mut().filter(|t| t.active) {
            None => Self::submit(
                io,
                send_token,
                Op::Send {
                    sock,
                    buf,
                    off,
                    len,
                    zero_copy: len >= zero_copy_min,
                },
            ),
            Some(t) => {
                (t.src, t.src_off, t.src_len, t.sealed) = (buf, off, len, 0);
                t.tx_type = CONTENT_APPLICATION_DATA;
                t.zero_copy_min = zero_copy_min;
                t.seal_batch(io, sock, send_token);
            }
        }
    }

    /// Send a TLS alert: two bytes, the level and the description. Completes
    /// like [`Stream::send`]. Does nothing useful on a plain stream, which
    /// has no alerts; the two bytes of `buf` at `off` are the scratch space.
    pub fn send_alert<IO: Io>(
        &mut self,
        io: &mut IO,
        buf: BufId,
        off: u32,
        level: u8,
        description: u8,
    ) {
        let (sock, send_token) = (self.sock, self.send_token);
        io.buf(buf)[off as usize..off as usize + 2].copy_from_slice(&[level, description]);
        match self.tls.as_mut().filter(|t| t.active) {
            None => Self::submit(
                io,
                send_token,
                Op::Send {
                    sock,
                    buf,
                    off,
                    len: 0,
                    zero_copy: false,
                },
            ),
            Some(t) => {
                (t.src, t.src_off, t.src_len, t.sealed) = (buf, off, 2, 0);
                t.tx_type = CONTENT_ALERT;
                t.zero_copy_min = u32::MAX;
                t.seal_batch(io, sock, send_token);
            }
        }
    }

    pub fn on_send<IO: Io>(&mut self, io: &mut IO, result: i32) -> Step {
        let (sock, send_token) = (self.sock, self.send_token);
        let Some(t) = self.tls.as_mut().filter(|t| t.active) else {
            return Step::Done(result);
        };
        // A send of nothing completes with zero: that happens only when
        // sealing failed before the first record of a batch.
        if result < 0 || (result == 0 && t.cipher_len > 0) {
            return Step::Done(result);
        }
        t.cipher_sent += result as u32;
        if t.cipher_sent < t.cipher_len {
            Self::submit(
                io,
                send_token,
                Op::Send {
                    sock,
                    buf: t.tx_buf,
                    off: t.cipher_sent,
                    len: t.cipher_len - t.cipher_sent,
                    zero_copy: false,
                },
            );
            return Step::Pending;
        }
        // Everything sealed so far is on the wire. If sealing had to stop,
        // say so now: the connection cannot carry the rest.
        if let Some(e) = t.tx_error.take() {
            return Step::Tls(TlsError::Record(e));
        }
        if t.sealed < t.src_len {
            t.seal_batch(io, sock, send_token);
            return Step::Pending;
        }
        Step::Done(t.src_len as i32)
    }

    // ---- receive ---------------------------------------------------------

    /// Receive up to `max` bytes into `buf` at `off`.
    pub fn recv<IO: Io>(&mut self, io: &mut IO, buf: BufId, off: u32, max: u32) -> Step {
        let (sock, recv_token) = (self.sock, self.recv_token);
        match self.tls.as_mut().filter(|t| t.active) {
            None => {
                Self::submit(
                    io,
                    recv_token,
                    Op::Recv {
                        sock,
                        buf,
                        off,
                        len: max,
                    },
                );
                Step::Pending
            }
            Some(t) => {
                (t.dst, t.dst_off, t.dst_max) = (buf, off, max);
                t.pump_rx(io, sock, recv_token)
            }
        }
    }

    pub fn on_recv<IO: Io>(&mut self, io: &mut IO, result: i32) -> Step {
        let (sock, recv_token) = (self.sock, self.recv_token);
        let Some(t) = self.tls.as_mut().filter(|t| t.active) else {
            return Step::Done(result);
        };
        if result <= 0 {
            return Step::Done(result);
        }
        t.rx_end += result as u32;
        t.pump_rx(io, sock, recv_token)
    }
}
