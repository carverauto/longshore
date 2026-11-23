// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Building and sending one attempt of an operation, in the store vendor's request form.

use super::exact::Cause;
use super::head::{gcs_headers, s3_headers, Bufs, Head, Headers, Text};
use super::*;

impl S3 {
    /// Sign and send one attempt for slot `i`.
    pub(super) fn attempt<IO: Io>(&mut self, io: &mut IO, i: usize) {
        if self.past_deadline(io, i) {
            return self.end_unanswered(i, Cause::Deadline);
        }
        let Some(addr) = self.address() else {
            return self.wait_for_address(io, i);
        };
        let s = self.slots[i];
        let date = amz_date(io.unix_secs());
        let mut text = Text::new();
        let head = Head::of(&s, self.cfg.vendor, &mut text);
        // Sent with the credentials in use now. A request already sent keeps
        // what it carries; only later attempts see a rotation.
        let creds = &self.creds[if s.candidate {
            1 - self.active
        } else {
            self.active
        }];
        let mut bufs = Bufs::new();
        let mut headers: Headers = [("", b""); sigv4::HEADERS_MAX];
        let n = if self.cfg.vendor == Vendor::Gcs {
            gcs_headers(&head, creds.secret_key(), &mut bufs, &mut headers)
        } else {
            let exact = &mut self.slots[i].exact;
            s3_headers(
                &head,
                &self.cfg,
                creds,
                &date,
                exact,
                &mut bufs,
                &mut headers,
            )
        };
        let req = Request {
            method: head.method,
            target: head.target,
            host: &self.cfg.host,
            host_id: 0,
            addr,
            headers: &headers[..n],
            body: s.body.filter(|_| !s.comparing),
            // A stat's buffer is for a copy of the head, not for a body.
            response_buf: s.response_buf.filter(|_| s.kind != Kind::Stat),
        };

        match self.client.start(io, i as u64, &req) {
            Ok(()) => {
                self.slots[i].attempts += 1;
                self.slots[i].state = State::InFlight;
            }
            // Every connection is busy: try again shortly, without spending an attempt.
            Err(Error::Busy) => self.wait(io, i, self.cfg.backoff_ns),
            Err(e) => self.fail_unsent(i, Fatal::Transport(e)),
        }
    }

    /// The address to send to: the fixed one, or the resolver's current answer.
    fn address(&mut self) -> Option<SocketAddr> {
        match &mut self.cfg.endpoint {
            Endpoint::Fixed(a) => Some(*a),
            Endpoint::Resolved(r) => r.lookup(0).map(|ip| SocketAddr::new(ip, self.cfg.port)),
        }
    }

    /// No address yet: wait a little rather than fail. This does not count
    /// as an attempt, but it is not allowed to go on for ever: a name that
    /// never resolves would otherwise hold the slot for good. The limit is
    /// the time the operation's attempts could have taken.
    fn wait_for_address<IO: Io>(&mut self, io: &mut IO, i: usize) {
        let waits_max = (self
            .cfg
            .http
            .request_timeout_ns
            .saturating_mul(u64::from(self.cfg.attempts_max))
            / self.cfg.backoff_ns.max(1))
        .max(1);
        if u64::from(self.slots[i].address_waits) >= waits_max {
            return self.fail_unsent(i, Fatal::NoAddress);
        }
        self.slots[i].address_waits += 1;
        self.wait(io, i, self.cfg.backoff_ns);
    }
}
