// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The `aws-lc` provider: AWS-LC's scatter/gather AES-GCM, which seals from
//! the caller's buffer and opens into it without a copy, with the content
//! type passed as a separate suffix.

use super::{Suite, HEADER_LEN, TAG_LEN};
use aws_lc_sys::{
    EVP_AEAD_CTX_cleanup, EVP_AEAD_CTX_init, EVP_AEAD_CTX_open_gather, EVP_AEAD_CTX_seal_scatter,
    EVP_AEAD_CTX_zero, EVP_aead_aes_128_gcm, EVP_aead_aes_256_gcm, EVP_AEAD_CTX,
};
use core::mem::MaybeUninit;

/// The cipher context, held inline so that keying allocates nothing.
pub(super) struct AwsLc {
    ctx: MaybeUninit<EVP_AEAD_CTX>,
    keyed: bool,
}

impl AwsLc {
    pub(super) fn new() -> AwsLc {
        AwsLc {
            ctx: MaybeUninit::zeroed(),
            keyed: false,
        }
    }

    /// Key the context; `false` if AWS-LC refuses the key.
    pub(super) fn init(&mut self, suite: Suite, key: &[u8]) -> bool {
        // Before the first key: the fastest AES-GCM routine this CPU can run.
        let _: bool = crate::wide_gcm::enable();
        self.wipe();
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
        self.keyed = ok == 1;
        self.keyed
    }

    /// Seal `content || content_type` into `body` (the encrypted content) and
    /// `tail` (the encrypted type byte, then the tag).
    pub(super) fn seal(
        &self,
        nonce: &[u8; 12],
        header: [u8; HEADER_LEN],
        content: &[u8],
        content_type: u8,
        body: &mut [u8],
        tail: &mut [u8],
    ) -> bool {
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
        ok == 1 && tail_len == 1 + TAG_LEN
    }

    /// Decrypt `ciphertext` into `dst`, which holds at least as many bytes.
    pub(super) fn open(
        &self,
        nonce: &[u8; 12],
        header: &[u8],
        ciphertext: &[u8],
        tag: &[u8],
        dst: &mut [u8],
    ) -> bool {
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
        ok == 1
    }

    /// Decrypt `body` where it lies.
    pub(super) fn open_in_place(
        &self,
        nonce: &[u8; 12],
        header: [u8; HEADER_LEN],
        body: &mut [u8],
        tag: &[u8],
    ) -> bool {
        let p = body.as_mut_ptr();
        // SAFETY: input and output are the same `body.len()` bytes, which
        // AWS-LC permits when the two pointers are equal; the tag and header
        // are separate live slices.
        let ok = unsafe {
            EVP_AEAD_CTX_open_gather(
                self.ctx.as_ptr(),
                p,
                nonce.as_ptr(),
                nonce.len(),
                p,
                body.len(),
                tag.as_ptr(),
                TAG_LEN,
                header.as_ptr(),
                HEADER_LEN,
            )
        };
        ok == 1
    }

    /// Erase the key schedule.
    pub(super) fn wipe(&mut self) {
        if self.keyed {
            // SAFETY: the context was initialized by `init`.
            unsafe { EVP_AEAD_CTX_cleanup(self.ctx.as_mut_ptr()) };
            self.keyed = false;
        }
        // The library's cleanup releases nothing for AES-GCM and leaves the
        // expanded key in the context, so wipe it here. Volatile writes, so
        // the compiler cannot drop them as dead stores.
        super::wipe::wipe_bytes(&mut self.ctx);
    }

    /// Whether any byte of the context is non-zero. For tests of wiping.
    #[cfg(test)]
    pub(super) fn holds_key_material(&self) -> bool {
        super::wipe::any_byte_set(&self.ctx)
    }
}
