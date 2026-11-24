// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `rustls` provider: rustls's own record encryption, built from the
//! negotiated suite with its ring provider.
//!
//! rustls's encrypter allocates a buffer for each record it seals and the
//! record is copied out of it, so sealing runs inside `allow_alloc`, the
//! exception the handshake uses. Its decrypter opens the record where it
//! lies and allocates nothing. Its keys cannot be copied, so a direction
//! holds either the encrypter (the sending side) or the decrypter.

use super::Suite;
use longshore_common::io::alloc_guard::allow_alloc;
use rustls::crypto::cipher::{
    AeadKey, InboundOpaqueMessage, Iv, MessageDecrypter, MessageEncrypter, OutboundChunks,
    OutboundPlainMessage, Tls13AeadAlgorithm,
};
use rustls::crypto::ring::cipher_suite::{TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384};
use rustls::{ContentType, ProtocolVersion};

/// Which way a rustls-keyed direction goes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Role {
    /// Seals what is sent.
    Seal,
    /// Opens what is received.
    Open,
}

enum Keys {
    None,
    Seal(Box<dyn MessageEncrypter>),
    Open(Box<dyn MessageDecrypter>),
}

pub(super) struct Rustls(Keys);

fn algorithm(suite: Suite) -> Option<&'static dyn Tls13AeadAlgorithm> {
    let s = match suite {
        Suite::Aes128Gcm => TLS13_AES_128_GCM_SHA256,
        Suite::Aes256Gcm => TLS13_AES_256_GCM_SHA384,
    };
    s.tls13().map(|s| s.aead_alg)
}

impl Rustls {
    pub(super) fn new() -> Rustls {
        Rustls(Keys::None)
    }

    pub(super) fn is_keyed(&self) -> bool {
        !matches!(self.0, Keys::None)
    }

    /// Key it for `role`. `key` must have the suite's key length.
    pub(super) fn init(&mut self, suite: Suite, key: AeadKey, iv: &[u8; 12], role: Role) -> bool {
        self.wipe();
        let Some(alg) = algorithm(suite) else {
            return false;
        };
        if key.as_ref().len() != suite.key_len() {
            return false;
        }
        let iv = Iv::from(*iv);
        self.0 = allow_alloc(|| match role {
            Role::Seal => Keys::Seal(alg.encrypter(key, iv)),
            Role::Open => Keys::Open(alg.decrypter(key, iv)),
        });
        true
    }

    /// Seal `content` as a record of `content_type` into `dst[..total]`, the
    /// whole record, header included.
    pub(super) fn seal(
        &mut self,
        seq: u64,
        content: &[u8],
        content_type: u8,
        dst: &mut [u8],
        total: usize,
    ) -> bool {
        let Keys::Seal(enc) = &mut self.0 else {
            return false;
        };
        let msg = OutboundPlainMessage {
            typ: ContentType::from(content_type),
            version: ProtocolVersion::TLSv1_2,
            payload: OutboundChunks::Single(content),
        };
        allow_alloc(|| match enc.encrypt(msg, seq) {
            Ok(record) => {
                let bytes = record.encode();
                if bytes.len() != total {
                    return false;
                }
                dst[..total].copy_from_slice(&bytes);
                true
            }
            Err(_) => false,
        })
    }

    /// Open a record's encrypted part, ciphertext then tag, where it lies.
    /// On success the content is at `payload[..len]`; returns its type and
    /// length.
    pub(super) fn open(&mut self, seq: u64, payload: &mut [u8]) -> Option<(u8, usize)> {
        let Keys::Open(dec) = &mut self.0 else {
            return None;
        };
        let msg = InboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            payload,
        );
        let plain = dec.decrypt(msg, seq).ok()?;
        Some((u8::from(plain.typ), plain.payload.len()))
    }

    /// Drop the keys, inside the allocation exception (dropping frees).
    pub(super) fn wipe(&mut self) {
        if self.is_keyed() {
            allow_alloc(|| self.0 = Keys::None);
        }
    }
}
