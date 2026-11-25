// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS 1.3 handshake by rustls, sans-I/O.
//!
//! The owner feeds received bytes to [`Handshake::read`] and sends whatever
//! [`Handshake::write`] produces, until [`Handshake::is_done`]. Then
//! [`Handshake::finish`] moves the negotiated traffic keys into two
//! [`Direction`]s and rustls is gone; every later record is handled by
//! `record`.
//!
//! rustls allocates while handshaking. Each call here runs inside
//! `allow_alloc`, the single sanctioned exception to the rule that nothing
//! allocates after initialization.

use crate::provider::Provider;
use crate::record::{Direction, RecordError, Suite, HEADER_LEN};
use longshore_common::io::alloc_guard::allow_alloc;
use rustls::crypto::cipher::AeadKey;
use rustls::{ClientConnection, ConnectionTrafficSecrets, ServerConnection};

pub use setup::{ClientSetup, ServerSetup};

mod setup;

/// Why TLS failed.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TlsError {
    /// The peer's certificate chain or name did not validate.
    BadCertificate,
    /// The handshake failed for another reason: an alert, a protocol
    /// violation, no common parameters.
    Handshake,
    /// A received record is malformed.
    BadRecord,
    /// The certificate, key or server name given at setup is unusable.
    Setup,
    /// `finish` was called before the handshake completed.
    NotDone,
    /// A cipher suite other than AES-GCM was negotiated.
    UnsupportedSuite,
    /// The peer sent an alert other than an orderly close, after the
    /// handshake. The value is the alert's description code.
    Alert(u8),
    /// The record layer refused a record.
    Record(RecordError),
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "rustls::Error is foreign and non_exhaustive: every other error is a failed handshake"
)]
fn map(e: &rustls::Error) -> TlsError {
    match e {
        rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented => {
            TlsError::BadCertificate
        }
        _ => TlsError::Handshake,
    }
}

enum Conn {
    Client(ClientConnection),
    Server(ServerConnection),
}

/// One handshake in progress. Dropping it frees rustls's memory, inside the
/// allocation exception.
pub struct Handshake(Option<Conn>, Provider);

impl Drop for Handshake {
    fn drop(&mut self) {
        allow_alloc(|| drop(self.0.take()));
    }
}

/// Length of the record at the start of `buf` during the handshake, when
/// records may still be unprotected: `None` if it is not all there yet.
fn whole_record(buf: &[u8]) -> Result<Option<usize>, TlsError> {
    let Some(h) = buf.get(..HEADER_LEN) else {
        return Ok(None);
    };
    // change_cipher_spec, alert, handshake, application_data.
    if !(20..=23).contains(&h[0]) || h[1] != 3 {
        return Err(TlsError::BadRecord);
    }
    let len = HEADER_LEN + u16::from_be_bytes([h[3], h[4]]) as usize;
    if len > HEADER_LEN + 16384 + 256 {
        return Err(TlsError::BadRecord);
    }
    Ok((buf.len() >= len).then_some(len))
}

impl Handshake {
    /// Begin a client handshake for `server_name`, which the certificate must match.
    pub fn client(setup: &ClientSetup, server_name: &str) -> Result<Handshake, TlsError> {
        allow_alloc(|| {
            let name = rustls::pki_types::ServerName::try_from(server_name.to_owned())
                .map_err(|_| TlsError::Setup)?;
            ClientConnection::new(setup.config(), name)
                .map(|c| Handshake(Some(Conn::Client(c)), setup.provider()))
                .map_err(|_| TlsError::Setup)
        })
    }

    /// Begin a server handshake, presenting the certificate in `setup`.
    pub fn server(setup: &ServerSetup) -> Result<Handshake, TlsError> {
        allow_alloc(|| {
            ServerConnection::new(setup.config())
                .map(|c| Handshake(Some(Conn::Server(c)), setup.provider()))
                .map_err(|_| TlsError::Setup)
        })
    }

    // The connection is taken out only by `finish`, which consumes the
    // handshake, and by `drop`: every other method finds it there.
    #[expect(
        clippy::expect_used,
        reason = "only finish and drop take the connection, and both end the handshake"
    )]
    fn conn(&self) -> &Conn {
        self.0.as_ref().expect("present until finish or drop")
    }

    #[expect(
        clippy::expect_used,
        reason = "only finish and drop take the connection, and both end the handshake"
    )]
    fn conn_mut(&mut self) -> &mut Conn {
        self.0.as_mut().expect("present until finish or drop")
    }

    fn is_handshaking(&self) -> bool {
        match self.conn() {
            Conn::Client(c) => c.is_handshaking(),
            Conn::Server(c) => c.is_handshaking(),
        }
    }

    fn wants_write(&self) -> bool {
        match self.conn() {
            Conn::Client(c) => c.wants_write(),
            Conn::Server(c) => c.wants_write(),
        }
    }

    /// True when nothing more needs to be received or sent before `finish`.
    pub fn is_done(&self) -> bool {
        !self.is_handshaking() && !self.wants_write()
    }

    /// Feed received bytes. Returns how many were consumed.
    ///
    /// Only whole records are consumed, and none once the handshake has
    /// completed, so bytes that belong to the record layer (a session ticket
    /// split across reads, early response data) are left for it.
    pub fn read(&mut self, input: &[u8]) -> Result<usize, TlsError> {
        allow_alloc(|| {
            let mut used = 0;
            while self.is_handshaking() {
                let Some(len) = whole_record(&input[used..])? else {
                    break;
                };
                let mut rec = &input[used..used + len];
                // rustls takes a bounded amount per call (4 KiB at the time
                // of writing), so one record can need several calls. Feeding
                // must continue until the whole record is in, or its tail is
                // silently lost and both sides wait for each other.
                while !rec.is_empty() {
                    let before = rec.len();
                    let r = match self.conn_mut() {
                        Conn::Client(c) => c
                            .read_tls(&mut rec)
                            .map_err(|_| TlsError::Handshake)
                            .and_then(|_| c.process_new_packets().map(|_| ()).map_err(|e| map(&e))),
                        Conn::Server(c) => c
                            .read_tls(&mut rec)
                            .map_err(|_| TlsError::Handshake)
                            .and_then(|_| c.process_new_packets().map(|_| ()).map_err(|e| map(&e))),
                    };
                    r?;
                    if rec.len() == before {
                        // No progress: rustls will not take more of this record.
                        return Err(TlsError::Handshake);
                    }
                }
                used += len;
            }
            Ok(used)
        })
    }

    /// Write pending handshake bytes into `out`. Returns how many were written;
    /// call again after sending them if `is_done` is still false.
    pub fn write(&mut self, out: &mut [u8]) -> usize {
        allow_alloc(|| {
            let mut cursor = std::io::Cursor::new(out);
            while self.wants_write() && (cursor.position() as usize) < cursor.get_ref().len() {
                let n = match self.conn_mut() {
                    Conn::Client(c) => c.write_tls(&mut cursor),
                    Conn::Server(c) => c.write_tls(&mut cursor),
                };
                if !matches!(n, Ok(n) if n > 0) {
                    break;
                }
            }
            cursor.position() as usize
        })
    }

    /// Move the traffic keys into the record layer. `tx` seals what we send,
    /// `rx` opens what we receive.
    pub fn finish(mut self, tx: &mut Direction, rx: &mut Direction) -> Result<(), TlsError> {
        if !self.is_done() {
            return Err(TlsError::NotDone);
        }
        allow_alloc(|| {
            let secrets = match self.0.take().ok_or(TlsError::Handshake)? {
                Conn::Client(c) => c.dangerous_extract_secrets(),
                Conn::Server(c) => c.dangerous_extract_secrets(),
            }
            .map_err(|_| TlsError::Handshake)?;
            let provider = self.1;
            for (dir, (seq, secret), sending) in [(tx, secrets.tx, true), (rx, secrets.rx, false)] {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "ConnectionTrafficSecrets is foreign and non_exhaustive: any suite but AES-GCM is unsupported"
                )]
                let (suite, key, iv) = match secret {
                    ConnectionTrafficSecrets::Aes128Gcm { key, iv } => (Suite::Aes128Gcm, key, iv),
                    ConnectionTrafficSecrets::Aes256Gcm { key, iv } => (Suite::Aes256Gcm, key, iv),
                    _ => return Err(TlsError::UnsupportedSuite),
                };
                let iv: &[u8; 12] = iv.as_ref().try_into().map_err(|_| TlsError::Handshake)?;
                install(dir, provider, suite, key, iv, seq, sending).map_err(TlsError::Record)?;
            }
            Ok(())
        })
    }
}

/// Key `dir` for `provider`: rustls's record encryption takes rustls's key
/// as it is, and seals or opens by the direction; the others take its bytes.
fn install(
    dir: &mut Direction,
    provider: Provider,
    suite: Suite,
    key: AeadKey,
    iv: &[u8; 12],
    seq: u64,
    sending: bool,
) -> Result<(), RecordError> {
    #[cfg(feature = "rustls")]
    if provider == Provider::Rustls {
        let role = if sending {
            crate::record::Role::Seal
        } else {
            crate::record::Role::Open
        };
        return dir.set_rustls_keys(suite, key, iv, seq, role);
    }
    let _: bool = sending;
    let keyed = dir.set_keys_with(provider, suite, key.as_ref(), iv, seq);
    // rustls wipes its copy of the key when it is dropped.
    drop(key);
    keyed
}
