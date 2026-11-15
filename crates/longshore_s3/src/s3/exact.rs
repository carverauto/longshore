// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `put`, `head` and `get`: operations that report what the store answered
//! instead of deciding what it means, within a limit of attempts and time.

use super::*;
use crate::multipart::Fixed;
use crate::report::{
    base64_decode, Answer, ContentRange, ETag, End, Expect, Limit, Put, ServerEncryption, Signed,
    VersionId,
};
pub(super) use crate::report::{Cause, Encryption};

/// What such an operation asked for, and how far it has got.
#[derive(Clone, Copy, Default)]
pub(super) struct Exact {
    attempts_max: u32,
    deadline: Option<u64>,
    /// The deadline passed while a request was in progress, and the
    /// request has been asked to stop.
    expired: bool,
    pub(super) if_match: Option<ETag>,
    pub(super) encryption: Option<Encryption>,
    /// The request of the attempt in progress.
    pub(super) signing: Option<Signed>,
    /// The last request handed to the `Io` for sending.
    request: Option<Signed>,
    sent: u32,
    unanswered: u32,
    /// The store's answer to the last request sent, if it gave one.
    answer: Option<Answer>,
}

/// The evidence of a request as it was signed.
pub(super) fn describe(method: &'static str, date: &[u8; 16], req: &sigv4::Request) -> Signed {
    let mut names = [0u8; crate::report::SIGNED_HEADERS_MAX];
    let n = sigv4::signed_headers(req, &mut names)
        .expect("the names of sixteen headers this client sets fit");
    Signed {
        method,
        date: *date,
        canonical_request_sha256: sigv4::canonical_request_sha256(req)
            .expect("the request was signed, so it is within the limits"),
        signed_headers: Fixed::new(&names[..n]).expect("a request signs its host at least"),
    }
}

impl S3 {
    /// Store `len` bytes of `body` at `key` as `options` say, within
    /// `limit`. The outcome is always [`Outcome::Report`].
    ///
    /// Nothing is decided here: a refusal because the object exists is a
    /// report whose answer has status 412, and the comparison that
    /// [`S3::store_no_clobber`] makes after a refused retry is not made.
    #[allow(clippy::too_many_arguments)]
    pub fn put<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        body: BufId,
        len: u32,
        options: &Put,
        limit: Limit,
    ) -> Result<(), SubmitError> {
        let kind = Kind::Store {
            no_clobber: options.no_clobber,
        };
        let asked = Exact {
            encryption: options.encryption,
            ..Exact::default()
        };
        let body = Some((body, len));
        self.submit_exact(
            io,
            token,
            key,
            kind,
            body,
            None,
            options.checksum,
            None,
            asked,
            limit,
        )
    }

    /// Ask the store about `key` without fetching it, within `limit`. The
    /// outcome is always [`Outcome::Report`]: an object that is not there
    /// is an answer with status 404, and one that is not the object
    /// `expect` names is an answer with status 412.
    pub fn head<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        expect: &Expect,
        limit: Limit,
    ) -> Result<(), SubmitError> {
        let asked = Exact {
            if_match: expect.etag,
            ..Exact::default()
        };
        let none = Checksum::None;
        self.submit_exact(
            io,
            token,
            key,
            Kind::Stat,
            None,
            None,
            none,
            expect.version.as_ref(),
            asked,
            limit,
        )
    }

    /// Fetch the object `expect` names, or the inclusive byte `range` of
    /// it, into `into`, within `limit`. The outcome is always
    /// [`Outcome::Report`]. A ranged fetch that the store answers with the
    /// whole object is an answer with status 200, and the range a store
    /// did serve is in the answer for the caller to compare.
    #[allow(clippy::too_many_arguments)]
    pub fn get<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        range: Option<(u64, u64)>,
        expect: &Expect,
        into: BufId,
        limit: Limit,
    ) -> Result<(), SubmitError> {
        if range.is_some_and(|(first, last)| first > last) {
            return Err(SubmitError::BadRange);
        }
        let asked = Exact {
            if_match: expect.etag,
            ..Exact::default()
        };
        let none = Checksum::None;
        self.submit_exact(
            io,
            token,
            key,
            Kind::Fetch { range },
            None,
            Some(into),
            none,
            expect.version.as_ref(),
            asked,
            limit,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_exact<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        kind: Kind,
        body: Option<(BufId, u32)>,
        response_buf: Option<BufId>,
        checksum: Checksum,
        version: Option<&VersionId>,
        mut asked: Exact,
        limit: Limit,
    ) -> Result<(), SubmitError> {
        if self.cfg.vendor != Vendor::S3 {
            return Err(SubmitError::Unsupported);
        }
        // A header value is visible ASCII; anything else could not be sent
        // as it was given.
        let visible = |tag: &ETag| tag.as_bytes().iter().all(|b| (b' '..=b'~').contains(b));
        if !asked.if_match.as_ref().is_none_or(visible) {
            return Err(SubmitError::BadKey);
        }
        let mut query = [0u8; QUERY_MAX];
        let mut query_len = 0;
        if let Some(version) = version {
            let name = b"versionId=";
            query[..name.len()].copy_from_slice(name);
            query_len = name.len()
                + uri_encode(version.as_bytes(), true, &mut query[name.len()..])
                    .ok_or(SubmitError::BadKey)?;
        }
        let query = &query[..query_len];
        let i = self.prepare(io, token, key, kind, body, response_buf, checksum, query)?;
        asked.attempts_max = limit.attempts.clamp(1, self.cfg.attempts_max);
        asked.deadline = limit.deadline_ns;
        self.slots[i].exact = Some(asked);
        self.attempt(io, i);
        Ok(())
    }

    /// Whether the request in progress or last sent under `token` was
    /// handed to the `Io` for sending, for an owner that must end the
    /// operation itself because the `Io` has failed.
    pub fn abandoned(&self, token: u64) -> Option<Report> {
        let (i, s) = self.slots[..self.cfg.requests_max as usize]
            .iter()
            .enumerate()
            .find(|(_, s)| {
                s.token == token && matches!(s.state, State::InFlight | State::Waiting { .. })
            })?;
        let mut exact = s.exact?;
        if s.state == State::InFlight && self.client.sent(i as u64) {
            exact.sent += 1;
            exact.unanswered += 1;
            exact.request = exact.signing;
            exact.answer = None;
        }
        Some(exact.report(Cause::Closed))
    }

    pub(super) fn past_deadline<IO: Io>(&self, io: &mut IO, i: usize) -> bool {
        self.slots[i]
            .exact
            .and_then(|exact| exact.deadline)
            .is_some_and(|deadline| io.now_ns() >= deadline)
    }

    /// End slot `i` with no answer to its last attempt.
    pub(super) fn end_unanswered(&mut self, i: usize, cause: Cause) {
        let exact = self.slots[i].exact.expect("only such operations end so");
        self.finish(i, Outcome::Report(exact.report(cause)));
    }

    /// End slot `i` because its attempt could not begin.
    pub(super) fn fail_unsent(&mut self, i: usize, fatal: Fatal) {
        if self.slots[i].exact.is_none() {
            return self.finish(i, Outcome::Fatal(fatal));
        }
        self.end_unanswered(
            i,
            match fatal {
                Fatal::Transport(e) => Cause::Transport(e),
                _ => Cause::NoAddress,
            },
        );
    }

    /// End operations whose deadline has passed: at once if nothing of
    /// theirs is in progress, otherwise once their request has stopped.
    pub(super) fn expire<IO: Io>(&mut self, io: &mut IO, now: u64) {
        for i in 0..self.cfg.requests_max as usize {
            let s = self.slots[i];
            let Some(exact) = s.exact else { continue };
            if exact.expired || !exact.deadline.is_some_and(|deadline| now >= deadline) {
                continue;
            }
            match s.state {
                State::Waiting { .. } => self.end_unanswered(i, Cause::Deadline),
                State::InFlight => {
                    self.slots[i].exact = Some(Exact {
                        expired: true,
                        ..exact
                    });
                    if let Handled::Done(d) = self.client.cancel(io, i as u64) {
                        self.on_attempt_done(io, i, d.result, d.sent);
                    }
                }
                State::Free | State::Finished => {}
            }
        }
    }

    pub(super) fn on_exact_done<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        result: Result<client::Response, Error>,
        sent: bool,
    ) {
        let s = self.slots[i];
        let mut exact = s.exact.expect("called for such operations only");
        if sent {
            exact.sent += 1;
            exact.request = exact.signing;
        }
        exact.signing = None;
        let answer = result.ok().map(|r| self.answer_of(io, &s, r));
        if sent {
            exact.unanswered += answer.is_none() as u32;
            exact.answer = answer;
        }
        let mut class = classify(&result);
        // Bytes that do not match the store's checksum were damaged on the
        // way, and fetching again may bring the right ones.
        if answer.is_some_and(|a| a.verified == Some(false)) {
            class = Class::Retry;
        }
        self.slots[i].exact = Some(exact);
        let again = !exact.expired && s.attempts < exact.attempts_max && !self.past_deadline(io, i);
        match class {
            Class::Retry if again => return self.attempt(io, i),
            Class::RetryDelayed if again => {
                let delay = (self.cfg.backoff_ns << s.delays.min(16)).min(self.cfg.backoff_max_ns);
                self.slots[i].delays += 1;
                return self.wait(io, i, delay);
            }
            _ => {}
        }
        let cause = match result {
            // A request stopped for the deadline ends as a timeout.
            Err(Error::Timeout) if exact.expired => Cause::Deadline,
            Err(e) => Cause::Transport(e),
            // Not used: an answer is what the operation ends with.
            Ok(_) => Cause::Closed,
        };
        self.finish(i, Outcome::Report(exact.report(cause)));
    }

    /// What the response on `r.conn` said, for the operation in slot `s`.
    fn answer_of<IO: Io>(&self, io: &mut IO, s: &Slot, r: client::Response) -> Answer {
        let mut malformed = false;
        // A header that is there but does not fit or parse is reported as
        // such, not as absent.
        let etag = present(self.client.header(io, r.conn, "etag"), &mut malformed).map(Fixed::new);
        let version = present(
            self.client.header(io, r.conn, "x-amz-version-id"),
            &mut malformed,
        )
        .map(Fixed::new);
        let request_id = present(
            self.client.header(io, r.conn, "x-amz-request-id"),
            &mut malformed,
        )
        .map(Fixed::new);
        let content_range = present(
            self.client.header(io, r.conn, "content-range"),
            &mut malformed,
        )
        .map(ContentRange::parse);
        let encryption = present(
            self.client
                .header(io, r.conn, "x-amz-server-side-encryption"),
            &mut malformed,
        )
        .map(|v| match v {
            b"AES256" => ServerEncryption::Aes256,
            _ => ServerEncryption::Other,
        });
        let sha256 = present(
            self.client.header(io, r.conn, "x-amz-checksum-sha256"),
            &mut malformed,
        )
        .map(base64_decode);
        let crc64 = present(
            self.client.header(io, r.conn, CHECKSUM_HEADER),
            &mut malformed,
        )
        .map(|v| <[u8; 12]>::try_from(v).ok());
        malformed |= etag == Some(None)
            || version == Some(None)
            || request_id == Some(None)
            || content_range == Some(None)
            || sha256 == Some(None)
            || crc64 == Some(None);
        let mut answer = Answer {
            status: r.status,
            etag: etag.flatten(),
            version: version.flatten(),
            request_id: request_id.flatten(),
            content_length: self.client.content_length(r.conn),
            content_range: content_range.flatten(),
            encryption,
            sha256: sha256.flatten(),
            crc64: crc64.flatten(),
            body_len: s.response_buf.map_or(0, |_| r.body_len),
            verified: None,
            malformed,
        };
        if let (Kind::Fetch { range: None }, 200, Some(theirs), Some(buf)) =
            (s.kind, r.status, answer.crc64, s.response_buf)
        {
            let body = &io.buf(buf)[..r.body_len as usize];
            let ours = longshore_crc::crc64_header_value(longshore_crc::crc64_nvme(body));
            answer.verified = Some(ours == theirs);
        }
        answer
    }
}

/// A header's value if it has one. A header sent with nothing in it is
/// noted as malformed and read as absent.
fn present<'a>(value: Option<&'a [u8]>, malformed: &mut bool) -> Option<&'a [u8]> {
    *malformed |= value.is_some_and(|v| v.is_empty());
    value.filter(|v| !v.is_empty())
}

impl Exact {
    /// How the operation ended. The last request sent decides: its answer
    /// if it got one, otherwise `cause`, which is also why nothing was sent
    /// if nothing was.
    fn report(&self, cause: Cause) -> Report {
        let end = match (self.sent, self.answer) {
            (0, _) => End::NotSent(cause),
            (_, Some(answer)) => End::Answered(answer),
            (_, None) => End::Unknown(cause),
        };
        Report {
            end,
            request: self.request,
            sent: self.sent,
            unanswered: self.unanswered,
        }
    }
}
