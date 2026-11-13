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

use crate::io::alloc_guard::allow_alloc;
use crate::tls::record::{Direction, RecordError, Suite, HEADER_LEN};
use rustls::crypto::aws_lc_rs as provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{
    ClientConfig, ClientConnection, ConnectionTrafficSecrets, RootCertStore, ServerConfig,
    ServerConnection,
};
use std::io::Write;
use std::sync::Arc;

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
    Record(RecordError),
}

fn map(e: rustls::Error) -> TlsError {
    match e {
        rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented => {
            TlsError::BadCertificate
        }
        _ => TlsError::Handshake,
    }
}

/// TLS 1.3 with the two AES-GCM suites and nothing else, AES-128 first.
/// With `aes128_only`, just the suite every TLS 1.3 peer must implement.
fn crypto(aes128_only: bool) -> Arc<rustls::crypto::CryptoProvider> {
    let mut p = provider::default_provider();
    p.cipher_suites.retain(|s| match s.suite() {
        rustls::CipherSuite::TLS13_AES_128_GCM_SHA256 => true,
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384 => !aes128_only,
        _ => false,
    });
    p.cipher_suites
        .sort_by_key(|s| s.suite() != rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    Arc::new(p)
}

/// Client configuration, built once at initialization.
pub struct ClientSetup(Arc<ClientConfig>);

impl ClientSetup {
    /// Trust the Mozilla root set compiled in through `webpki-roots`.
    pub fn with_public_roots() -> ClientSetup {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::build(roots, false)
    }

    /// As [`ClientSetup::with_public_roots`], but offering only AES-128-GCM,
    /// the faster of the two suites and the one TLS 1.3 requires every peer
    /// to support.
    pub fn with_public_roots_aes128() -> ClientSetup {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::build(roots, true)
    }

    /// Trust exactly these DER-encoded certificates as roots.
    pub fn with_roots(der: &[&[u8]]) -> Result<ClientSetup, TlsError> {
        let mut roots = RootCertStore::empty();
        for cert in der {
            roots
                .add(CertificateDer::from(cert.to_vec()))
                .map_err(|_| TlsError::Setup)?;
        }
        Ok(Self::build(roots, false))
    }

    fn build(roots: RootCertStore, aes128_only: bool) -> ClientSetup {
        let mut config = ClientConfig::builder_with_provider(crypto(aes128_only))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 is supported by the provider")
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.enable_secret_extraction = true;
        // No tickets are stored: every connection does a full handshake.
        config.resumption = rustls::client::Resumption::disabled();
        ClientSetup(Arc::new(config))
    }
}

/// Server configuration for the in-tree test server.
pub struct ServerSetup(Arc<ServerConfig>);

impl ServerSetup {
    /// `chain` is the certificate chain, leaf first; `key` is its PKCS#8 private key. Both DER.
    pub fn new(chain: &[&[u8]], key: &[u8]) -> Result<ServerSetup, TlsError> {
        let chain: Vec<CertificateDer<'static>> = chain
            .iter()
            .map(|c| CertificateDer::from(c.to_vec()))
            .collect();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
        let mut config = ServerConfig::builder_with_provider(crypto(false))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 is supported by the provider")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|_| TlsError::Setup)?;
        config.enable_secret_extraction = true;
        Ok(ServerSetup(Arc::new(config)))
    }
}

enum Conn {
    Client(ClientConnection),
    Server(ServerConnection),
}

/// One handshake in progress. Dropping it frees rustls's memory, inside the
/// allocation exception.
pub struct Handshake(Option<Conn>);

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
            ClientConnection::new(setup.0.clone(), name)
                .map(|c| Handshake(Some(Conn::Client(c))))
                .map_err(|_| TlsError::Setup)
        })
    }

    pub fn server(setup: &ServerSetup) -> Result<Handshake, TlsError> {
        allow_alloc(|| {
            ServerConnection::new(setup.0.clone())
                .map(|c| Handshake(Some(Conn::Server(c))))
                .map_err(|_| TlsError::Setup)
        })
    }

    fn conn(&self) -> &Conn {
        self.0.as_ref().expect("present until finish or drop")
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
                    let r = match self.0.as_mut().expect("present until finish or drop") {
                        Conn::Client(c) => c
                            .read_tls(&mut rec)
                            .map_err(|_| TlsError::Handshake)
                            .and_then(|_| c.process_new_packets().map(|_| ()).map_err(map)),
                        Conn::Server(c) => c
                            .read_tls(&mut rec)
                            .map_err(|_| TlsError::Handshake)
                            .and_then(|_| c.process_new_packets().map(|_| ()).map_err(map)),
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
                let n = match self.0.as_mut().expect("present until finish or drop") {
                    Conn::Client(c) => c.write_tls(&mut cursor),
                    Conn::Server(c) => c.write_tls(&mut cursor),
                };
                if !matches!(n, Ok(n) if n > 0) {
                    break;
                }
            }
            let _ = cursor.flush();
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
            let secrets = match self.0.take().expect("present until finish or drop") {
                Conn::Client(c) => c.dangerous_extract_secrets(),
                Conn::Server(c) => c.dangerous_extract_secrets(),
            }
            .map_err(|_| TlsError::Handshake)?;
            for (dir, (seq, secret)) in [(tx, secrets.tx), (rx, secrets.rx)] {
                match &secret {
                    ConnectionTrafficSecrets::Aes128Gcm { key, iv } => dir.set_keys(
                        Suite::Aes128Gcm,
                        key.as_ref(),
                        iv.as_ref().try_into().map_err(|_| TlsError::Handshake)?,
                        seq,
                    ),
                    ConnectionTrafficSecrets::Aes256Gcm { key, iv } => dir.set_keys(
                        Suite::Aes256Gcm,
                        key.as_ref(),
                        iv.as_ref().try_into().map_err(|_| TlsError::Handshake)?,
                        seq,
                    ),
                    _ => return Err(TlsError::UnsupportedSuite),
                }
                .map_err(TlsError::Record)?;
            }
            Ok(())
        })
    }
}
