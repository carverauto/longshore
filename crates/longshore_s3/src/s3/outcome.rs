// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Deciding what a finished attempt means: success, a retry, or a fatal outcome.

use super::checksum::Checked;
use super::*;
use std::ops::ControlFlow;

impl S3 {
    pub(super) fn on_attempt_done<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        result: Result<client::Response, Error>,
        sent: bool,
    ) {
        let s = self.slots[i];
        if s.exact.is_some() {
            return self.on_exact_done(io, i, result, sent);
        }
        // A completion's response was lost and the retry found the upload
        // gone. The object at the key decides: ours if its checksum is.
        if s.kind == Kind::Complete && s.comparing {
            return self.on_completion_compared(io, i, &s, result);
        }
        let mut decided = None;
        // Statuses with a meaning specific to the operation.
        if let Ok(r) = result {
            match self.on_status(io, i, &s, r) {
                ControlFlow::Break(()) => return,
                ControlFlow::Continue(class) => decided = class,
            }
        }
        let class = decided.unwrap_or_else(|| classify(&result));
        let class = self.transient(io, &s, result, class);
        let check = self.check_body(io, &s, result, class);
        self.conclude(io, i, &s, result, check);
    }

    /// The answer to the comparison after a completion's response was lost.
    fn on_completion_compared<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        s: &Slot,
        result: Result<client::Response, Error>,
    ) {
        #[expect(
            clippy::expect_used,
            reason = "a completion compares only when it was sent with a checksum"
        )]
        let ours = self.checksum_value(s.crc.expect("only completions with a checksum compare"));
        let class = classify(&result);
        if matches!(class, Class::Retry | Class::RetryDelayed) {
            if s.attempts < self.cfg.attempts_max {
                return self.attempt(io, i);
            }
            // The comparison could not be made, so whether the upload
            // completed is not known. A 404 would say it did not.
            return self.finish(i, &Outcome::Fatal(Fatal::RetriesExhausted));
        }
        let same = matches!(result, Ok(r) if r.status() == 200
            && self.stored_checksum(io, r.conn()) == Some(ours));
        let outcome = if same {
            Outcome::Stored
        } else {
            Outcome::Fatal(Fatal::Status(404))
        };
        self.finish(i, &outcome);
    }

    /// Statuses with a meaning specific to the operation. Breaks when the
    /// operation finished or was attempted again; otherwise carries the
    /// class a multipart step decided, if any.
    fn on_status<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        s: &Slot,
        r: client::Response,
    ) -> ControlFlow<(), Option<Class>> {
        let mut decided = None;
        if r.status() == 200 {
            match self.multipart_outcome(io, s.kind, r, s.crc) {
                Some(Ok(outcome)) => {
                    self.finish(i, &outcome);
                    return ControlFlow::Break(());
                }
                Some(Err(class)) => decided = Some(class),
                None => {}
            }
        }
        let outcome = match (s.kind, r.status()) {
            // The upload is gone after an earlier attempt was sent: that
            // attempt may have completed it.
            (Kind::Complete, 404)
                if s.attempts > 1 && s.crc.is_some() && s.attempts < self.cfg.attempts_max =>
            {
                self.slots[i].comparing = true;
                self.store_comparisons += 1;
                self.attempt(io, i);
                return ControlFlow::Break(());
            }
            (Kind::Store { .. }, _) => {
                return match self.on_store_status(io, i, s, r) {
                    ControlFlow::Break(()) => ControlFlow::Break(()),
                    ControlFlow::Continue(()) => ControlFlow::Continue(decided),
                };
            }
            // Nothing left to abort.
            (Kind::Abort, 404) => Outcome::Aborted,
            (Kind::Stat, 200) => Outcome::Stat(self.stat_of(io, s.response_buf, r.conn())),
            // A copy of something that is not there.
            (Kind::Stat | Kind::Copy, 404) | (Kind::Fetch { .. }, 404) => Outcome::NotFound,
            // A server may ignore Range and send the whole object. Those are
            // not the bytes that were asked for.
            (Kind::Fetch { range: Some(_) }, status)
                if (200..300).contains(&status) && status != 206 =>
            {
                Outcome::Fatal(Fatal::RangeIgnored)
            }
            // Google Cloud Storage answers an erase of an absent object, or the
            // retry of one whose response was lost, with 404 NoSuchKey.
            // The code is read as the document's code: a store may
            // repeat the key in the document, and a key can be called anything.
            (Kind::Erase, 404)
                if multipart::error_code_in(self.client.discarded_body(io, r.conn()))
                    == Some(b"NoSuchKey") =>
            {
                Outcome::Erased
            }
            _ => return ControlFlow::Continue(decided),
        };
        self.finish(i, &outcome);
        ControlFlow::Break(())
    }

    /// A store's statuses: the answer to a comparison, and a refusal of a
    /// conditional store. Breaks when the operation finished or was
    /// attempted again.
    fn on_store_status<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        s: &Slot,
        r: client::Response,
    ) -> ControlFlow<()> {
        let outcome = match (s.kind, r.status()) {
            // The comparison's answer: the object at the key, and its checksum.
            (Kind::Store { .. }, 200) if s.comparing => {
                #[expect(
                    clippy::expect_used,
                    reason = "a store compares only when it was sent with a checksum"
                )]
                let ours = self.checksum_value(s.crc.expect("only stores with a checksum compare"));
                match self.stored_checksum(io, r.conn()) {
                    Some(theirs) if theirs == ours => Outcome::Stored,
                    // Someone else's object, not ours.
                    Some(_) => Outcome::AlreadyExists { after_retry: false },
                    // The store keeps no checksum: the caller has to decide.
                    None => Outcome::AlreadyExists { after_retry: true },
                }
            }
            // The object went away between the refusal and the comparison: store it again.
            (Kind::Store { .. }, 404) if s.comparing && s.attempts < self.cfg.attempts_max => {
                self.slots[i].comparing = false;
                self.attempt(io, i);
                return ControlFlow::Break(());
            }
            // A refusal on the first attempt is someone else's object. After a
            // retry it may be ours, written by an attempt whose response was lost.
            (Kind::Store { no_clobber: true }, STATUS_PRECONDITION_FAILED) => {
                if s.attempts > 1 && s.crc.is_some() && s.attempts < self.cfg.attempts_max {
                    self.slots[i].comparing = true;
                    self.store_comparisons += 1;
                    self.attempt(io, i);
                    return ControlFlow::Break(());
                }
                Outcome::AlreadyExists {
                    after_retry: s.attempts > 1,
                }
            }
            // The object went away and there are no attempts left to store it again.
            (Kind::Store { .. }, 404) if s.comparing => Outcome::Fatal(Fatal::RetriesExhausted),
            _ => return ControlFlow::Continue(()),
        };
        self.finish(i, &outcome);
        ControlFlow::Break(())
    }

    /// Answers that are transient although their status alone says otherwise.
    fn transient<IO: Io>(
        &self,
        io: &mut IO,
        s: &Slot,
        result: Result<client::Response, Error>,
        class: Class,
    ) -> Class {
        let Ok(r) = result else {
            return class;
        };
        match (s.kind, r.status()) {
            // Another conditional write to the key is in progress: possibly
            // our own earlier attempt, still being committed.
            (Kind::Store { no_clobber: true }, 409) if !s.comparing => Class::RetryDelayed,
            // The store gave up waiting for the body.
            (kind, 400)
                if kind.stores_bytes()
                    && !s.comparing
                    && self
                        .client
                        .discarded_body(io, r.conn())
                        .windows(14)
                        .any(|w| w == b"RequestTimeout") =>
            {
                Class::Retry
            }
            _ => class,
        }
    }

    /// Finish, retry or delay the operation as its class says.
    fn conclude<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        s: &Slot,
        result: Result<client::Response, Error>,
        checked: Checked,
    ) {
        let Checked {
            class,
            verified,
            mismatch,
        } = checked;
        // The comparison could not be completed, but the refusal that led to it
        // already proved an object is there, and it may be ours. Reporting a
        // plain failure would lose that: a caller who tried again would be told
        // the object is someone else's.
        if s.comparing
            && (class == Class::Fatal
                || (class != Class::Success && s.attempts >= self.cfg.attempts_max))
        {
            return self.finish(i, &Outcome::AlreadyExists { after_retry: true });
        }
        match class {
            Class::Success => self.finish(i, &succeeded(s, result, verified)),
            Class::Fatal => self.finish(
                i,
                &Outcome::Fatal(match result {
                    Ok(r) => Fatal::Status(r.status()),
                    Err(e) => Fatal::Transport(e),
                }),
            ),
            _ if s.attempts >= self.cfg.attempts_max => self.finish(
                i,
                &Outcome::Fatal(if mismatch {
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
}

/// The outcome of an attempt that succeeded.
fn succeeded(s: &Slot, result: Result<client::Response, Error>, verified: bool) -> Outcome {
    match s.kind {
        // A comparison that succeeded returned above; any other
        // success status says nothing about whose object it is.
        Kind::Store { .. } if s.comparing => Outcome::AlreadyExists { after_retry: true },
        Kind::Store { .. } | Kind::Complete => Outcome::Stored,
        Kind::Abort => Outcome::Aborted,
        Kind::Copy => Outcome::Copied,
        Kind::EraseMany => Outcome::ErasedMany {
            len: result.map_or(0, |r| r.body_len()),
        },
        // Their success is decided above, from a 200 response.
        Kind::Begin { .. } | Kind::Part | Kind::Stat => Outcome::Fatal(Fatal::BadResponse),
        Kind::Fetch { .. } => Outcome::Fetched {
            len: result.map_or(0, |r| r.body_len()),
            verified,
        },
        Kind::Erase => Outcome::Erased,
        Kind::List => Outcome::Listed {
            len: result.map_or(0, |r| r.body_len()),
        },
    }
}
