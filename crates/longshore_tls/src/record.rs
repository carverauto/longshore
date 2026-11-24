// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS 1.3 record protection (RFC 8446 section 5) with AES-GCM.
//!
//! A protected record on the wire is:
//!
//! ```text
//! | type 0x17 | version 0x0303 | length (2) | ciphertext (length - 16) | tag (16) |
//! ```
//!
//! and the ciphertext decrypts to `content || content type (1) || zero padding`.
//! The five-byte header is the additional authenticated data, and the nonce is
//! the connection's IV XORed with the record sequence number.
//!
//! Three providers can seal and open records (see [`Provider`]): AWS-LC
//! reads the content from one buffer and writes the record to another in a
//! single pass, with the content type handed to the cipher as a separate
//! suffix, and opens from the receive buffer straight into the destination;
//! ring works in place, copying each body once; rustls's own record
//! encryption allocates a buffer per sealed record.

pub use crate::provider::Provider;
use frame::INNER_MAX;
pub use frame::{
    record_len, RecordError, Suite, CONTENT_ALERT, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE,
    FRAGMENT_MAX, HEADER_LEN, KEY_RECORDS_MAX, KEY_RECORDS_RETIRE, OVERHEAD, RECORD_MAX, TAG_LEN,
};
#[cfg(feature = "rustls")]
pub use with_rustls::Role;

#[cfg(not(all(feature = "aws-lc", feature = "ring", feature = "rustls")))]
use absent::*;
#[cfg(feature = "aws-lc")]
use with_aws_lc::AwsLc;
#[cfg(feature = "ring")]
use with_ring::Ring;
#[cfg(feature = "rustls")]
use with_rustls::Rustls;

#[cfg(not(all(feature = "aws-lc", feature = "ring", feature = "rustls")))]
mod absent;
mod frame;
#[cfg(any(feature = "aws-lc", feature = "ring"))]
mod wipe;
#[cfg(feature = "aws-lc")]
mod with_aws_lc;
#[cfg(feature = "ring")]
mod with_ring;
#[cfg(feature = "rustls")]
mod with_rustls;

/// One direction's keys and record counter.
///
/// The cipher state lives inside this value. Key it in place with
/// [`Direction::set_keys`] once it is where it will stay (a slot in a table
/// allocated at initialization), and do not move it afterwards.
pub struct Direction {
    aws_lc: AwsLc,
    ring: Ring,
    rustls: Rustls,
    /// The provider holding the keys, if any.
    keyed: Option<Provider>,
    iv: [u8; 12],
    seq: u64,
}

impl Direction {
    /// An unkeyed direction. Sealing or opening with it fails.
    pub fn new() -> Direction {
        Direction {
            aws_lc: AwsLc::new(),
            ring: Ring::new(),
            rustls: Rustls::new(),
            keyed: None,
            iv: [0; 12],
            seq: 0,
        }
    }

    /// Install traffic keys for the preferred provider of AWS-LC and ring
    /// (see [`Direction::set_keys_with`]), starting at record sequence
    /// number `seq`. Does not allocate.
    pub fn set_keys(
        &mut self,
        suite: Suite,
        key: &[u8],
        iv: &[u8; 12],
        seq: u64,
    ) -> Result<(), RecordError> {
        let provider = if cfg!(feature = "aws-lc") {
            Provider::AwsLc
        } else {
            Provider::Ring
        };
        self.set_keys_with(provider, suite, key, iv, seq)
    }

    /// Install traffic keys for `provider`, starting at record sequence
    /// number `seq`. Does not allocate.
    ///
    /// Fails with [`RecordError::BadKey`] for a provider that is not compiled
    /// in, and for [`Provider::Rustls`], whose keys come from a handshake
    /// (see `set_rustls_keys`).
    pub fn set_keys_with(
        &mut self,
        provider: Provider,
        suite: Suite,
        key: &[u8],
        iv: &[u8; 12],
        seq: u64,
    ) -> Result<(), RecordError> {
        if key.len() != suite.key_len() {
            return Err(RecordError::BadKey);
        }
        self.clear();
        let ok = match provider {
            Provider::AwsLc => self.aws_lc.init(suite, key),
            Provider::Ring => self.ring.init(suite, key),
            Provider::Rustls => false,
        };
        self.keyed(ok, provider, iv, seq)
    }

    /// Install rustls's record encryption for `role`, with a key as rustls
    /// hands it out ([`rustls::ConnectionTrafficSecrets`]). Allocates, inside
    /// the allocation exception: rustls boxes its keys.
    #[cfg(feature = "rustls")]
    pub fn set_rustls_keys(
        &mut self,
        suite: Suite,
        key: rustls::crypto::cipher::AeadKey,
        iv: &[u8; 12],
        seq: u64,
        role: Role,
    ) -> Result<(), RecordError> {
        self.clear();
        let ok = self.rustls.init(suite, key, iv, role);
        self.keyed(ok, Provider::Rustls, iv, seq)
    }

    fn keyed(
        &mut self,
        ok: bool,
        provider: Provider,
        iv: &[u8; 12],
        seq: u64,
    ) -> Result<(), RecordError> {
        if !ok {
            self.clear();
            return Err(RecordError::BadKey);
        }
        self.keyed = Some(provider);
        self.iv = *iv;
        self.seq = seq;
        Ok(())
    }

    /// The provider holding the keys, if any.
    pub fn provider(&self) -> Option<Provider> {
        self.keyed
    }

    /// Erase the keys.
    pub fn clear(&mut self) {
        self.aws_lc.wipe();
        self.ring.wipe();
        self.rustls.wipe();
        self.keyed = None;
        self.iv = [0; 12];
    }

    /// Whether any byte of key material remains. For tests of [`Direction::clear`].
    #[cfg(test)]
    fn holds_key_material(&self) -> bool {
        self.aws_lc.holds_key_material()
            || self.ring.holds_key_material()
            || self.rustls.is_keyed()
            || self.iv != [0; 12]
    }

    /// Records processed so far.
    pub fn sequence(&self) -> u64 {
        self.seq
    }

    fn nonce(&self) -> [u8; 12] {
        let mut n = self.iv;
        for (b, s) in n[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *b ^= s;
        }
        n
    }

    /// Seal `content` as one record of type `content_type` into `dst`.
    /// Returns the record length, `content.len() + OVERHEAD`.
    ///
    /// At most [`FRAGMENT_MAX`] bytes; with AWS-LC, read once and not
    /// copied. `content_type` must not be zero.
    pub fn seal(
        &mut self,
        content_type: u8,
        content: &[u8],
        dst: &mut [u8],
    ) -> Result<usize, RecordError> {
        let provider = self.keyed.ok_or(RecordError::BadKey)?;
        // A zero type would read as padding: the receiver would take the
        // content's last non-zero byte for the type.
        if content_type == 0 {
            return Err(RecordError::NoContentType);
        }
        if content.len() > FRAGMENT_MAX {
            return Err(RecordError::BadLength);
        }
        let total = content.len() + OVERHEAD;
        if dst.len() < total {
            return Err(RecordError::DestinationTooSmall);
        }
        if self.seq >= KEY_RECORDS_MAX {
            return Err(RecordError::SequenceExhausted);
        }
        let encrypted_len = (content.len() + 1 + TAG_LEN) as u16;
        let header = [
            CONTENT_APPLICATION_DATA,
            3,
            3,
            (encrypted_len >> 8) as u8,
            encrypted_len as u8,
        ];
        dst[..HEADER_LEN].copy_from_slice(&header);
        let nonce = self.nonce();
        let rest = &mut dst[HEADER_LEN..total];
        let ok = match provider {
            Provider::AwsLc => {
                let (body, tail) = rest.split_at_mut(content.len());
                self.aws_lc
                    .seal(&nonce, header, content, content_type, body, tail)
            }
            Provider::Ring => self.ring.seal(&nonce, header, content, content_type, rest),
            Provider::Rustls => self
                .rustls
                .seal(self.seq, content, content_type, dst, total),
        };
        if !ok {
            return Err(RecordError::BadRecord);
        }
        self.seq += 1;
        Ok(total)
    }

    /// Check a complete record before opening it: its length, the
    /// sequence limit, and that keys are installed.
    fn check(&self, record: &[u8]) -> Result<Provider, RecordError> {
        let provider = self.keyed.ok_or(RecordError::BadKey)?;
        if record_len(record)? != Some(record.len()) {
            return Err(RecordError::BadLength);
        }
        if record.len() - HEADER_LEN - TAG_LEN > INNER_MAX {
            return Err(RecordError::BadLength);
        }
        if self.seq >= KEY_RECORDS_MAX {
            return Err(RecordError::SequenceExhausted);
        }
        Ok(provider)
    }

    /// Bytes [`Direction::open`] needs in its destination for a record of
    /// `record_len` bytes: the content plus its type byte and any padding,
    /// and with rustls, which decrypts the tag's bytes along, the tag too.
    pub fn open_room(&self, record_len: usize) -> usize {
        let room = record_len.saturating_sub(HEADER_LEN);
        if self.keyed == Some(Provider::Rustls) {
            room
        } else {
            room.saturating_sub(TAG_LEN)
        }
    }

    /// Open one complete record (as delimited by [`record_len`]) into `dst`.
    /// Returns its content type and content length.
    ///
    /// `dst` must hold [`Direction::open_room`] bytes: the content plus the
    /// content-type byte and any padding, that is `record.len() - HEADER_LEN -
    /// TAG_LEN`, and with rustls `TAG_LEN` more; the bytes after the content
    /// are scratch. On failure `dst` holds nothing meaningful and the keys
    /// must not be used again.
    pub fn open(&mut self, record: &[u8], dst: &mut [u8]) -> Result<(u8, usize), RecordError> {
        let provider = self.check(record)?;
        let room = self.open_room(record.len());
        if dst.len() < room {
            return Err(RecordError::DestinationTooSmall);
        }
        if provider == Provider::Rustls {
            let payload = &mut dst[..room];
            payload.copy_from_slice(&record[HEADER_LEN..]);
            return self.open_rustls(payload);
        }
        let nonce = self.nonce();
        let (header, rest) = record.split_at(HEADER_LEN);
        let (ciphertext, tag) = rest.split_at(room);
        let ok = match provider {
            Provider::AwsLc => self.aws_lc.open(&nonce, header, ciphertext, tag, dst),
            Provider::Ring | Provider::Rustls => {
                self.ring.open(&nonce, header, ciphertext, tag, dst)
            }
        };
        self.opened(ok, &dst[..room])
    }

    /// Open one complete record where it lies. On success the content is at
    /// `record[HEADER_LEN..HEADER_LEN + len]`. Used when the destination has
    /// no room for the record's content plus its trailing type byte.
    pub fn open_in_place(&mut self, record: &mut [u8]) -> Result<(u8, usize), RecordError> {
        let provider = self.check(record)?;
        if provider == Provider::Rustls {
            return self.open_rustls(&mut record[HEADER_LEN..]);
        }
        let nonce = self.nonce();
        let mut header = [0u8; HEADER_LEN];
        header.copy_from_slice(&record[..HEADER_LEN]);
        let ct_len = record.len() - HEADER_LEN - TAG_LEN;
        let (body, tag) = record[HEADER_LEN..].split_at_mut(ct_len);
        let ok = match provider {
            Provider::AwsLc => self.aws_lc.open_in_place(&nonce, header, body, tag),
            Provider::Ring | Provider::Rustls => {
                self.ring.open_in_place(&nonce, &header, body, tag)
            }
        };
        self.opened(ok, body)
    }

    /// rustls opens a record's encrypted part, ciphertext then tag, where
    /// it lies, leaving the content at its start.
    fn open_rustls(&mut self, payload: &mut [u8]) -> Result<(u8, usize), RecordError> {
        let (ctype, len) = self
            .rustls
            .open(self.seq, payload)
            .ok_or(RecordError::BadRecord)?;
        self.seq += 1;
        if ctype == 0 {
            return Err(RecordError::NoContentType);
        }
        if len > FRAGMENT_MAX {
            return Err(RecordError::BadLength);
        }
        Ok((ctype, len))
    }

    /// After AWS-LC or ring opened a record into `plain`: count it, strip
    /// the zero padding and read the content type, the last non-zero byte.
    fn opened(&mut self, ok: bool, plain: &[u8]) -> Result<(u8, usize), RecordError> {
        if !ok {
            return Err(RecordError::BadRecord);
        }
        self.seq += 1;
        let end = plain
            .iter()
            .rposition(|b| *b != 0)
            .ok_or(RecordError::NoContentType)?;
        if end > FRAGMENT_MAX {
            return Err(RecordError::BadLength);
        }
        Ok((plain[end], end))
    }
}

impl Default for Direction {
    fn default() -> Direction {
        Direction::new()
    }
}

impl Drop for Direction {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests;
