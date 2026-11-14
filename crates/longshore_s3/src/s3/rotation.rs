// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Rereading the credentials file and adopting new credentials once the store accepts them.

use super::*;

impl S3 {
    /// How many bytes a credentials read asks for.
    pub(super) fn credentials_read_len(&self) -> u32 {
        self.cfg.http.buffer_size.min(CREDENTIALS_FILE_MAX)
    }

    /// The credentials file was read (or not): decide whether to try what it holds.
    pub(super) fn on_credentials_read<IO: Io>(
        &mut self,
        io: &mut IO,
        f: CredentialsFile,
        result: i32,
    ) {
        let again = Provider::Idle {
            at: io.now_ns() + f.reread_ns,
        };
        // A read that filled the whole request may have cut the file short.
        let result = if result >= self.credentials_read_len() as i32 {
            -1
        } else {
            result
        };
        let text = &io.buf(f.buf)[..result.max(0) as usize];
        let parsed = match (result < 0, self.cfg.vendor) {
            (true, _) => None,
            (false, Vendor::S3) => Credentials::parse(text),
            (false, Vendor::Gcs) => Credentials::parse_bearer(text),
        };
        let Some(candidate) = parsed else {
            self.credential_failures += 1;
            self.provider = again;
            return;
        };
        if candidate == self.creds[self.active] {
            self.provider = again;
            return;
        }
        self.creds[1 - self.active] = candidate;
        let prefix = match f.validate {
            Validate::List(prefix) => prefix,
            // Adopted as they are, with nothing to prove them.
            Validate::Nothing => {
                self.active = 1 - self.active;
                self.credential_rotations += 1;
                self.provider = again;
                return;
            }
        };
        // Prove the new credentials work before any request depends on them.
        let slot = self.cfg.requests_max as usize;
        self.slots[slot].candidate = true;
        let from = submit::ListFrom::Start;
        match self.list_in_slot(io, slot, 0, prefix.as_str(), from, 1, None, f.buf) {
            Ok(()) => self.provider = Provider::Validating,
            Err(_) => {
                self.credential_failures += 1;
                self.provider = again;
            }
        }
    }

    /// Finish a validation if its request has completed.
    pub(super) fn check_validation<IO: Io>(&mut self, io: &mut IO) {
        let slot = self.cfg.requests_max as usize;
        let (Provider::Validating, State::Finished) = (self.provider, self.slots[slot].state)
        else {
            return;
        };
        let outcome = self.slots[slot].outcome;
        self.slots[slot].state = State::Free;
        if matches!(outcome, Outcome::Listed { .. }) {
            self.active = 1 - self.active;
            self.credential_rotations += 1;
        } else {
            self.credential_failures += 1;
        }
        let reread = self.cfg.credentials_file.map_or(0, |f| f.reread_ns);
        self.provider = Provider::Idle {
            at: io.now_ns() + reread,
        };
    }
}
