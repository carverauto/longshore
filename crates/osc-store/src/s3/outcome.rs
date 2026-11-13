// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Deciding what a finished attempt means: success, a retry, or a fatal outcome.

use super::*;

impl S3 {
    pub(super) fn on_attempt_done<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        result: Result<client::Response, Error>,
    ) {
        let s = self.slots[i];
        let finish = |me: &mut S3, o: Outcome| me.slots[i].state = State::Finished(o);
        // Statuses with a meaning specific to the operation.
        if let Ok(r) = result {
            match (s.kind, r.status) {
                // The comparison's answer: the object at the key, and its checksum.
                (Kind::Store { .. }, 200) if s.comparing => {
                    let ours =
                        self.checksum_value(s.crc.expect("only stores with a checksum compare"));
                    return match self.stored_checksum(io, r.conn) {
                        Some(theirs) if theirs == ours => finish(self, Outcome::Stored),
                        // Someone else's object, not ours.
                        Some(_) => finish(self, Outcome::AlreadyExists { after_retry: false }),
                        // The store keeps no checksum: the caller has to decide.
                        None => finish(self, Outcome::AlreadyExists { after_retry: true }),
                    };
                }
                // The object went away between the refusal and the comparison: store it again.
                (Kind::Store { .. }, 404) if s.comparing && s.attempts < self.cfg.attempts_max => {
                    self.slots[i].comparing = false;
                    return self.attempt(io, i);
                }
                // A refusal on the first attempt is someone else's object. After a
                // retry it may be ours, written by an attempt whose response was lost.
                (Kind::Store { no_clobber: true }, STATUS_PRECONDITION_FAILED) => {
                    if s.attempts > 1 && s.crc.is_some() && s.attempts < self.cfg.attempts_max {
                        self.slots[i].comparing = true;
                        self.store_comparisons += 1;
                        return self.attempt(io, i);
                    }
                    return finish(
                        self,
                        Outcome::AlreadyExists {
                            after_retry: s.attempts > 1,
                        },
                    );
                }
                // The object went away and there are no attempts left to store it again.
                (Kind::Store { .. }, 404) if s.comparing => {
                    return finish(self, Outcome::Fatal(Fatal::RetriesExhausted))
                }
                (Kind::Fetch { .. }, 404) => return finish(self, Outcome::NotFound),
                // A server may ignore Range and send the whole object. Those are
                // not the bytes that were asked for.
                (Kind::Fetch { range: Some(_) }, status)
                    if (200..300).contains(&status) && status != 206 =>
                {
                    return finish(self, Outcome::Fatal(Fatal::RangeIgnored))
                }
                // Google Cloud Storage answers an erase of an absent object, or the
                // retry of one whose response was lost, with 404 NoSuchKey.
                (Kind::Erase, 404)
                    if self
                        .client
                        .discarded_body(io, r.conn)
                        .windows(9)
                        .any(|w| w == b"NoSuchKey") =>
                {
                    return finish(self, Outcome::Erased)
                }
                _ => {}
            }
        }
        let mut class = classify(&result);
        // Answers that are transient although their status alone says otherwise.
        if let Ok(r) = result {
            match (s.kind, r.status) {
                // Another conditional write to the key is in progress: possibly
                // our own earlier attempt, still being committed.
                (Kind::Store { no_clobber: true }, 409) if !s.comparing => {
                    class = Class::RetryDelayed
                }
                // The store gave up waiting for the body.
                (Kind::Store { .. }, 400)
                    if !s.comparing
                        && self
                            .client
                            .discarded_body(io, r.conn)
                            .windows(14)
                            .any(|w| w == b"RequestTimeout") =>
                {
                    class = Class::Retry
                }
                _ => {}
            }
        }
        let mut verified = false;
        let mut mismatch = false;
        match (result, s.kind) {
            // A whole-object fetch: check the bytes against the store's checksum.
            (Ok(r), Kind::Fetch { range: None }) if class == Class::Success => {
                if let Some(expected) = self.stored_checksum(io, r.conn) {
                    let buf = s.response_buf.expect("fetches have a response buffer");
                    let body = &io.buf(buf)[..r.body_len as usize];
                    let actual = self.checksum_value(match self.cfg.vendor {
                        Vendor::S3 => osc_checksum::crc64_nvme(body),
                        Vendor::Gcs => osc_checksum::crc32c(body) as u64,
                    });
                    verified = actual == expected;
                    // Corrupted in transit: fetch again rather than hand back bad bytes.
                    if !verified {
                        mismatch = true;
                        class = Class::Retry;
                    }
                } else if self.cfg.require_checksums {
                    // The checksum itself is missing or mangled.
                    mismatch = true;
                    class = Class::Retry;
                }
            }
            // The store rejected our checksum: the body was damaged on the way.
            (Ok(r), Kind::Store { .. }) if r.status == 400 && s.crc.is_some() && !s.comparing => {
                let doc = self.client.discarded_body(io, r.conn);
                // S3 names it BadDigest (assumed; no enforcing S3 store has been
                // tried). Google Cloud Storage names it CrcMismatch (observed).
                let code: &[u8] = match self.cfg.vendor {
                    Vendor::S3 => b"BadDigest",
                    Vendor::Gcs => b"CrcMismatch",
                };
                if doc.windows(code.len()).any(|w| w == code) {
                    class = Class::Retry;
                }
            }
            _ => {}
        }
        // The comparison could not be completed, but the refusal that led to it
        // already proved an object is there, and it may be ours. Reporting a
        // plain failure would lose that: a caller who tried again would be told
        // the object is someone else's.
        if s.comparing
            && (class == Class::Fatal
                || (class != Class::Success && s.attempts >= self.cfg.attempts_max))
        {
            return finish(self, Outcome::AlreadyExists { after_retry: true });
        }
        match class {
            Class::Success => {
                let outcome = match s.kind {
                    // A comparison that succeeded returned above; any other
                    // success status says nothing about whose object it is.
                    Kind::Store { .. } if s.comparing => {
                        Outcome::AlreadyExists { after_retry: true }
                    }
                    Kind::Store { .. } => Outcome::Stored,
                    Kind::Fetch { .. } => Outcome::Fetched {
                        len: result.map_or(0, |r| r.body_len),
                        verified,
                    },
                    Kind::Erase => Outcome::Erased,
                    Kind::List => Outcome::Listed {
                        len: result.map_or(0, |r| r.body_len),
                    },
                };
                finish(self, outcome)
            }
            Class::Fatal => finish(
                self,
                Outcome::Fatal(match result {
                    Ok(r) => Fatal::Status(r.status),
                    Err(e) => Fatal::Transport(e),
                }),
            ),
            _ if s.attempts >= self.cfg.attempts_max => finish(
                self,
                Outcome::Fatal(if mismatch {
                    Fatal::ChecksumMismatch
                } else {
                    Fatal::RetriesExhausted
                }),
            ),
            Class::Retry => self.attempt(io, i),
            Class::RetryDelayed => {
                let delay = (self.cfg.backoff_ns << s.delays.min(16)).min(self.cfg.backoff_max_ns);
                self.slots[i].delays += 1;
                self.wait(io, i, delay);
            }
        }
    }

    /// A checksum as the store's header carries it, padded with zeros:
    /// twelve base64 characters for S3, eight for Google Cloud Storage.
    pub(super) fn checksum_value(&self, crc: u64) -> [u8; 12] {
        match self.cfg.vendor {
            Vendor::S3 => osc_checksum::crc64_header_value(crc),
            Vendor::Gcs => {
                let mut out = [0u8; 12];
                out[..8].copy_from_slice(&osc_checksum::crc32c_header_value(crc as u32));
                out
            }
        }
    }

    /// The checksum the store sent with the response on `conn`, in the form
    /// [`S3::checksum_value`] produces. `None` if there is none or it is malformed.
    pub(super) fn stored_checksum<IO: Io>(&self, io: &mut IO, conn: u32) -> Option<[u8; 12]> {
        let mut out = [0u8; 12];
        match self.cfg.vendor {
            Vendor::S3 => out.copy_from_slice(
                self.client
                    .header(io, conn, CHECKSUM_HEADER)
                    .filter(|v| v.len() == 12)?,
            ),
            Vendor::Gcs => {
                // "crc32c=AAAAAA==,md5=...": the parts may come in either order.
                let value = self.client.header(io, conn, "x-goog-hash")?;
                let part = value
                    .split(|b| *b == b',')
                    .map(|p| p.trim_ascii())
                    .find_map(|p| p.strip_prefix(b"crc32c="))?;
                out[..8].copy_from_slice(Some(part).filter(|p| p.len() == 8)?);
            }
        }
        Some(out)
    }
}
