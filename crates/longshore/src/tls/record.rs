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
//! Sealing reads the content from one buffer and writes the record to
//! another in a single pass: the content type is handed to the cipher as a
//! separate suffix, so the content is never copied in order to append it.
//! Opening likewise decrypts from the receive buffer straight into the
//! destination.

use aws_lc_sys::{
    EVP_AEAD_CTX_cleanup, EVP_AEAD_CTX_init, EVP_AEAD_CTX_open_gather, EVP_AEAD_CTX_seal_scatter,
    EVP_AEAD_CTX_zero, EVP_aead_aes_128_gcm, EVP_aead_aes_256_gcm, EVP_AEAD_CTX,
};
use core::mem::MaybeUninit;

pub const HEADER_LEN: usize = 5;
pub const TAG_LEN: usize = 16;
/// Largest content in one record.
pub const FRAGMENT_MAX: usize = 16384;
/// Largest decrypted record: content, its type byte and any padding
/// together (RFC 8446, section 5.4). A longer one is refused unopened.
const INNER_MAX: usize = FRAGMENT_MAX + 1;
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
    /// The decrypted record is all padding, with no content type.
    NoContentType,
    /// The destination cannot hold the record.
    DestinationTooSmall,
    /// The key has the wrong length for the suite.
    BadKey,
    /// These keys have protected [`KEY_RECORDS_MAX`] records and must not
    /// protect another.
    SequenceExhausted,
}

/// One direction's keys and record counter.
///
/// The cipher context lives inside this value. Key it in place with
/// [`Direction::set_keys`] once it is where it will stay (a slot in a table
/// allocated at initialization), and do not move it afterwards.
pub struct Direction {
    ctx: MaybeUninit<EVP_AEAD_CTX>,
    keyed: bool,
    iv: [u8; 12],
    seq: u64,
}

impl Direction {
    /// An unkeyed direction. Sealing or opening with it fails.
    pub fn new() -> Direction {
        Direction {
            ctx: MaybeUninit::zeroed(),
            keyed: false,
            iv: [0; 12],
            seq: 0,
        }
    }

    /// Install traffic keys, starting at record sequence number `seq`.
    /// Does not allocate.
    pub fn set_keys(
        &mut self,
        suite: Suite,
        key: &[u8],
        iv: &[u8; 12],
        seq: u64,
    ) -> Result<(), RecordError> {
        if key.len() != suite.key_len() {
            return Err(RecordError::BadKey);
        }
        self.clear();
        // SAFETY: `ctx` is owned storage of the right size and alignment.
        // `EVP_AEAD_CTX_zero` puts it in the state `EVP_AEAD_CTX_init`
        // expects, and for AES-GCM the context keeps its state inline.
        let ok = unsafe {
            let aead = match suite {
                Suite::Aes128Gcm => EVP_aead_aes_128_gcm(),
                Suite::Aes256Gcm => EVP_aead_aes_256_gcm(),
            };
            EVP_AEAD_CTX_zero(self.ctx.as_mut_ptr());
            EVP_AEAD_CTX_init(
                self.ctx.as_mut_ptr(),
                aead,
                key.as_ptr(),
                key.len(),
                TAG_LEN,
                core::ptr::null_mut(),
            )
        };
        if ok != 1 {
            return Err(RecordError::BadKey);
        }
        self.keyed = true;
        self.iv = *iv;
        self.seq = seq;
        Ok(())
    }

    /// Erase the keys.
    pub fn clear(&mut self) {
        if self.keyed {
            // SAFETY: the context was initialized by `set_keys`.
            unsafe { EVP_AEAD_CTX_cleanup(self.ctx.as_mut_ptr()) };
            self.keyed = false;
        }
        // The library's cleanup releases nothing for AES-GCM and leaves the
        // expanded key in the context, so wipe it here. Volatile writes, so
        // the compiler cannot drop them as dead stores.
        let bytes = self.ctx.as_mut_ptr().cast::<u8>();
        for i in 0..core::mem::size_of::<EVP_AEAD_CTX>() {
            // SAFETY: `bytes` covers exactly the context, which this type owns.
            unsafe { bytes.add(i).write_volatile(0) };
        }
        self.iv = [0; 12];
    }

    /// Whether any byte of key material remains. For tests of [`Direction::clear`].
    #[cfg(test)]
    fn holds_key_material(&self) -> bool {
        // SAFETY: the context is plain bytes owned by this type.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                self.ctx.as_ptr().cast::<u8>(),
                core::mem::size_of::<EVP_AEAD_CTX>(),
            )
        };
        bytes.iter().any(|b| *b != 0) || self.iv != [0; 12]
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
    /// `content` is read once and not copied. At most [`FRAGMENT_MAX`] bytes.
    pub fn seal(
        &mut self,
        content_type: u8,
        content: &[u8],
        dst: &mut [u8],
    ) -> Result<usize, RecordError> {
        if !self.keyed {
            return Err(RecordError::BadKey);
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
        let (header, rest) = dst[..total].split_at_mut(HEADER_LEN);
        header.copy_from_slice(&[
            CONTENT_APPLICATION_DATA,
            3,
            3,
            (encrypted_len >> 8) as u8,
            encrypted_len as u8,
        ]);
        let (body, tail) = rest.split_at_mut(content.len());
        let nonce = self.nonce();
        let suffix = [content_type];
        let mut tail_len = 0usize;
        // SAFETY: every pointer is to a live slice of the stated length.
        // `body` (output) and `content` (input) are distinct buffers; `tail`
        // receives the encrypted suffix byte followed by the tag.
        let ok = unsafe {
            EVP_AEAD_CTX_seal_scatter(
                self.ctx.as_ptr(),
                body.as_mut_ptr(),
                tail.as_mut_ptr(),
                &mut tail_len,
                tail.len(),
                nonce.as_ptr(),
                nonce.len(),
                content.as_ptr(),
                content.len(),
                suffix.as_ptr(),
                1,
                header.as_ptr(),
                HEADER_LEN,
            )
        };
        if ok != 1 || tail_len != 1 + TAG_LEN {
            return Err(RecordError::BadRecord);
        }
        self.seq += 1;
        Ok(total)
    }

    /// Open one complete record (as delimited by [`record_len`]) into `dst`.
    /// Returns its content type and content length.
    ///
    /// `dst` must have room for the content plus the content-type byte and
    /// any padding, that is `record.len() - HEADER_LEN - TAG_LEN` bytes; the
    /// bytes after the content are scratch. On failure `dst` holds nothing
    /// meaningful and the keys must not be used again.
    pub fn open(&mut self, record: &[u8], dst: &mut [u8]) -> Result<(u8, usize), RecordError> {
        if !self.keyed {
            return Err(RecordError::BadKey);
        }
        if record_len(record)? != Some(record.len()) {
            return Err(RecordError::BadLength);
        }
        let (header, rest) = record.split_at(HEADER_LEN);
        let (ciphertext, tag) = rest.split_at(rest.len() - TAG_LEN);
        if ciphertext.len() > INNER_MAX {
            return Err(RecordError::BadLength);
        }
        if dst.len() < ciphertext.len() {
            return Err(RecordError::DestinationTooSmall);
        }
        if self.seq >= KEY_RECORDS_MAX {
            return Err(RecordError::SequenceExhausted);
        }
        let nonce = self.nonce();
        // SAFETY: every pointer is to a live slice of the stated length, and
        // `dst` has at least `ciphertext.len()` bytes.
        let ok = unsafe {
            EVP_AEAD_CTX_open_gather(
                self.ctx.as_ptr(),
                dst.as_mut_ptr(),
                nonce.as_ptr(),
                nonce.len(),
                ciphertext.as_ptr(),
                ciphertext.len(),
                tag.as_ptr(),
                TAG_LEN,
                header.as_ptr(),
                HEADER_LEN,
            )
        };
        if ok != 1 {
            return Err(RecordError::BadRecord);
        }
        self.seq += 1;
        // Strip zero padding; the last non-zero byte is the content type.
        let plain = &dst[..ciphertext.len()];
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

impl Direction {
    /// Open one complete record where it lies. On success the content is at
    /// `record[HEADER_LEN..HEADER_LEN + len]`. Used when the destination has
    /// no room for the record's content plus its trailing type byte.
    pub fn open_in_place(&mut self, record: &mut [u8]) -> Result<(u8, usize), RecordError> {
        if !self.keyed {
            return Err(RecordError::BadKey);
        }
        if record_len(record)? != Some(record.len()) {
            return Err(RecordError::BadLength);
        }
        if self.seq >= KEY_RECORDS_MAX {
            return Err(RecordError::SequenceExhausted);
        }
        let nonce = self.nonce();
        let header: [u8; HEADER_LEN] = record[..HEADER_LEN]
            .try_into()
            .expect("checked by record_len");
        let ct_len = record.len() - HEADER_LEN - TAG_LEN;
        if ct_len > INNER_MAX {
            return Err(RecordError::BadLength);
        }
        let (body, tag) = record[HEADER_LEN..].split_at_mut(ct_len);
        let p = body.as_mut_ptr();
        // SAFETY: input and output are the same `ct_len` bytes, which AWS-LC
        // permits when the two pointers are equal; the tag and header are
        // separate live slices.
        let ok = unsafe {
            EVP_AEAD_CTX_open_gather(
                self.ctx.as_ptr(),
                p,
                nonce.as_ptr(),
                nonce.len(),
                p,
                ct_len,
                tag.as_ptr(),
                TAG_LEN,
                header.as_ptr(),
                HEADER_LEN,
            )
        };
        if ok != 1 {
            return Err(RecordError::BadRecord);
        }
        self.seq += 1;
        let end = body
            .iter()
            .rposition(|b| *b != 0)
            .ok_or(RecordError::NoContentType)?;
        if end > FRAGMENT_MAX {
            return Err(RecordError::BadLength);
        }
        Ok((body[end], end))
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

#[cfg(test)]
mod tests;
