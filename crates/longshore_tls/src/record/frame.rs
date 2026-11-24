// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The record format: sizes, limits, suites, errors, and finding where a
//! record ends.

pub const HEADER_LEN: usize = 5;
pub const TAG_LEN: usize = 16;
/// Largest content in one record.
pub const FRAGMENT_MAX: usize = 16384;
/// Largest decrypted record: content, its type byte and any padding
/// together (RFC 8446, section 5.4). A longer one is refused unopened.
pub(super) const INNER_MAX: usize = FRAGMENT_MAX + 1;
/// Bytes a record adds to its content: header, content type, tag.
pub const OVERHEAD: usize = HEADER_LEN + 1 + TAG_LEN;
/// Largest record this implementation produces or accepts.
pub const RECORD_MAX: usize = FRAGMENT_MAX + OVERHEAD;
/// Largest encrypted length a peer may send: content, type, tag, and up to
/// 255 bytes of allowance (RFC 8446 5.2).
const CIPHERTEXT_MAX: usize = FRAGMENT_MAX + 256;

pub const CONTENT_ALERT: u8 = 21;
pub const CONTENT_HANDSHAKE: u8 = 22;

/// The most records one set of traffic keys may protect, in each direction.
///
/// AES-GCM loses its security margin after about 2^24.5 full-size records
/// under one key (RFC 8446, section 5.5), roughly 390 GB. This layer does
/// not rotate keys with KeyUpdate; it refuses to go past 2^24 records, and
/// its users retire a connection at [`KEY_RECORDS_RETIRE`], well before
/// that, so that a new connection brings new keys.
pub const KEY_RECORDS_MAX: u64 = 1 << 24;
/// Past this many records in either direction a connection should carry no
/// further requests. Half the hard limit: about 137 GB of full-size records,
/// leaving more room than any single request can use.
pub const KEY_RECORDS_RETIRE: u64 = KEY_RECORDS_MAX / 2;
pub const CONTENT_APPLICATION_DATA: u8 = 23;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Suite {
    Aes128Gcm,
    Aes256Gcm,
}

impl Suite {
    pub fn key_len(self) -> usize {
        match self {
            Suite::Aes128Gcm => 16,
            Suite::Aes256Gcm => 32,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RecordError {
    /// The header is not a TLS 1.3 protected record.
    BadHeader,
    /// The declared length is larger than the protocol allows, or too small
    /// to hold a tag and a content type.
    BadLength,
    /// Authentication failed: the record was modified, or the keys or
    /// sequence number are wrong.
    BadRecord,
    /// The decrypted record is all padding, with no content type. Also what
    /// sealing answers when asked for a content type of zero, which a
    /// receiver could not tell from padding.
    NoContentType,
    /// The destination cannot hold the record.
    DestinationTooSmall,
    /// The key has the wrong length for the suite.
    BadKey,
    /// These keys have protected [`KEY_RECORDS_MAX`] records and must not
    /// protect another.
    SequenceExhausted,
}

/// Given the bytes received so far, the total length of the record at their
/// start: `Ok(None)` if the header is not complete yet.
pub fn record_len(buf: &[u8]) -> Result<Option<usize>, RecordError> {
    let Some(h) = buf.get(..HEADER_LEN) else {
        // Reject a wrong type or version as soon as those bytes are in.
        let ok = buf
            .iter()
            .zip([CONTENT_APPLICATION_DATA, 3, 3])
            .all(|(a, b)| *a == b);
        return if ok {
            Ok(None)
        } else {
            Err(RecordError::BadHeader)
        };
    };
    if h[..3] != [CONTENT_APPLICATION_DATA, 3, 3] {
        return Err(RecordError::BadHeader);
    }
    let len = u16::from_be_bytes([h[3], h[4]]) as usize;
    if !(1 + TAG_LEN..=CIPHERTEXT_MAX).contains(&len) {
        return Err(RecordError::BadLength);
    }
    Ok(Some(HEADER_LEN + len))
}
