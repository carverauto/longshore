// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `ring` provider. ring encrypts and decrypts only in place, so sealing
//! copies the content into the record buffer first, and opening into a
//! separate destination copies the ciphertext there first.

use super::{Suite, HEADER_LEN};
use core::mem::MaybeUninit;
use ring::aead::{Aad, LessSafeKey, Nonce, Tag, UnboundKey, AES_128_GCM, AES_256_GCM};

/// ring's key, held inline (for AES-GCM it owns no heap memory) so that it
/// can be wiped when it is dropped.
pub(super) struct Ring {
    key: MaybeUninit<LessSafeKey>,
    keyed: bool,
}

impl Ring {
    pub(super) fn new() -> Ring {
        Ring {
            key: MaybeUninit::zeroed(),
            keyed: false,
        }
    }

    fn key(&self) -> &LessSafeKey {
        debug_assert!(self.keyed);
        // SAFETY: only called on a keyed value, whose key `init` wrote.
        unsafe { self.key.assume_init_ref() }
    }

    /// Key it; `false` if ring refuses the key.
    pub(super) fn init(&mut self, suite: Suite, key: &[u8]) -> bool {
        self.wipe();
        let algorithm = match suite {
            Suite::Aes128Gcm => &AES_128_GCM,
            Suite::Aes256Gcm => &AES_256_GCM,
        };
        let Ok(unbound) = UnboundKey::new(algorithm, key) else {
            return false;
        };
        let _: &mut LessSafeKey = self.key.write(LessSafeKey::new(unbound));
        self.keyed = true;
        true
    }

    /// Seal `content || content_type` into `out`, which is the record after
    /// its header: `content.len() + 1 + TAG_LEN` bytes.
    pub(super) fn seal(
        &self,
        nonce: &[u8; 12],
        header: [u8; HEADER_LEN],
        content: &[u8],
        content_type: u8,
        out: &mut [u8],
    ) -> bool {
        let (inner, tag_out) = out.split_at_mut(content.len() + 1);
        inner[..content.len()].copy_from_slice(content);
        inner[content.len()] = content_type;
        let nonce = Nonce::assume_unique_for_key(*nonce);
        match self
            .key()
            .seal_in_place_separate_tag(nonce, Aad::from(header), inner)
        {
            Ok(tag) => {
                tag_out.copy_from_slice(tag.as_ref());
                true
            }
            Err(_) => false,
        }
    }

    /// Decrypt `ciphertext` into `dst`: copied there, then opened in place.
    pub(super) fn open(
        &self,
        nonce: &[u8; 12],
        header: &[u8],
        ciphertext: &[u8],
        tag: &[u8],
        dst: &mut [u8],
    ) -> bool {
        let out = &mut dst[..ciphertext.len()];
        out.copy_from_slice(ciphertext);
        self.open_in_place(nonce, header, out, tag)
    }

    /// Decrypt `body` where it lies.
    pub(super) fn open_in_place(
        &self,
        nonce: &[u8; 12],
        header: &[u8],
        body: &mut [u8],
        tag: &[u8],
    ) -> bool {
        let Ok(tag) = Tag::try_from(tag) else {
            return false;
        };
        let nonce = Nonce::assume_unique_for_key(*nonce);
        self.key()
            .open_in_place_separate_tag(nonce, Aad::from(header), tag, body, 0..)
            .is_ok()
    }

    /// Drop the key and erase its bytes.
    pub(super) fn wipe(&mut self) {
        if self.keyed {
            // SAFETY: the key was written by `init` and is dropped once.
            unsafe { self.key.assume_init_drop() };
            self.keyed = false;
        }
        super::wipe::wipe_bytes(&mut self.key);
    }

    /// Whether any byte of the key is non-zero. For tests of wiping.
    #[cfg(test)]
    pub(super) fn holds_key_material(&self) -> bool {
        // A keyed value's padding may be unset; only an unkeyed one, zeroed
        // or wiped, is read byte by byte.
        self.keyed || super::wipe::any_byte_set(&self.key)
    }
}
