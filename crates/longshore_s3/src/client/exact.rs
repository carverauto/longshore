// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `put`, `head` and `get` through the shared client: one request, or a
//! few, within a deadline, and a report of exactly what happened.

use super::lane::{Finished, Kind};
use super::{error_of, least_busy, Buffer, Client, Error, Pending};
use crate::report::{Expect, Limit, Put, Report};
use crate::s3::Outcome;
use longshore_common::io::Io;
use std::time::Instant;

/// How far one `put`, `head` or `get` may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Requests the operation may send, counting the first. Zero is read
    /// as one, and more than the client's own limit of four as four. With
    /// one, a request is signed once and sent at most once.
    pub attempts: u32,
    /// No request is started at or after this instant, and one in progress
    /// is cancelled at the I/O thread's next timer turn, which comes every
    /// 50 ms. The operation ends when the kernel has let go of the request,
    /// so it ends shortly after the deadline, not at it. The time a call
    /// spends waiting for a buffer or an operation slot is not bounded by
    /// this: a request that reaches the I/O thread after the deadline is
    /// simply not sent.
    pub deadline: Option<Instant>,
}

impl Budget {
    /// As many attempts as the client makes for its other operations, and
    /// no deadline.
    pub const UNLIMITED: Budget = Budget {
        attempts: u32::MAX,
        deadline: None,
    };

    /// One request, started before `deadline` or not at all.
    pub const fn once(deadline: Instant) -> Budget {
        Budget {
            attempts: 1,
            deadline: Some(deadline),
        }
    }

    /// This budget on the clock of the I/O thread that carries it out.
    pub(super) fn limit<IO: Io>(&self, io: &mut IO) -> Limit {
        Limit {
            attempts: self.attempts,
            deadline_ns: self.deadline.map(|deadline| {
                let left = deadline.saturating_duration_since(Instant::now());
                io.now_ns()
                    .saturating_add(left.as_nanos().min(u64::MAX as u128) as u64)
            }),
        }
    }
}

/// The report of an operation that ends with one. Anything else means
/// nothing was sent.
fn report_of(finished: &Finished, _: u32) -> Result<Report, Error> {
    match finished {
        Finished::Outcome(Outcome::Report(report)) => Ok(*report),
        other => Err(error_of(other)),
    }
}

impl Client {
    /// Store the first `len` bytes of `body` under `key` as `options` say,
    /// within `budget`, and report what the store answered.
    ///
    /// The result is `Err` only when nothing was sent: the key or length
    /// was refused, or the client was shut down. Otherwise it is a
    /// [`Report`], whatever the store's status. It comes, with the buffer,
    /// only once the I/O thread has finished with the operation and the
    /// kernel with the buffer, so the result is also the sign that nothing
    /// of the operation is still in progress. One exception: an I/O thread
    /// whose ring failed and could not be stopped gives the buffer back
    /// void, as [`Buffer`] describes.
    ///
    /// Nothing is decided for the caller. A store that refuses because an
    /// object exists answers 412, and with more than one attempt
    /// [`Report::unanswered`] tells whether an earlier request of this same
    /// call may be what wrote it.
    pub fn put(
        &self,
        key: &str,
        body: Buffer,
        len: usize,
        options: &Put,
        budget: Budget,
    ) -> Pending<(Result<Report, Error>, Buffer)> {
        let too_long = len > body.lane.buf_size;
        let (lane, buf, alive) = body.into_parts();
        let kind = Kind::Put { options: *options };
        let early = too_long.then_some(Error::TooLong);
        Pending::new(
            (lane, alive),
            key,
            kind,
            Some(buf),
            len as u32,
            early,
            report_of,
        )
        .within(budget, &Expect::default())
    }

    /// Ask the store about the object `expect` names under `key`, within
    /// `budget`, without fetching it. The result is as for [`Client::put`].
    /// No object is an answer with status 404, and an object other than
    /// the one expected is an answer with status 412.
    pub fn head(
        &self,
        key: &str,
        expect: &Expect,
        budget: Budget,
    ) -> Pending<Result<Report, Error>> {
        let first = self.lanes[least_busy(&self.lanes)].clone();
        let pending = Pending::new(
            (first, self.alive.clone()),
            key,
            Kind::Head,
            None,
            0,
            None,
            report_of,
        )
        .within(budget, expect);
        if self.lanes.len() > 1 {
            pending.on_any(self.lanes.clone())
        } else {
            pending
        }
    }

    /// Fetch the object `expect` names under `key`, or bytes `first..=last`
    /// of it, into `into`, within `budget`. The result is as for
    /// [`Client::put`]. The bytes are in the buffer, as many as the
    /// answer's `body_len`, and they are the object's only when its status
    /// is 200 or 206. Compare the answer's range with the one asked for: a
    /// store may serve less, and one that ignores ranges answers 200.
    pub fn get(
        &self,
        key: &str,
        range: Option<(u64, u64)>,
        expect: &Expect,
        into: Buffer,
        budget: Budget,
    ) -> Pending<(Result<Report, Error>, Buffer)> {
        let (lane, buf, alive) = into.into_parts();
        let kind = Kind::Get { range };
        Pending::new((lane, alive), key, kind, Some(buf), 0, None, report_of).within(budget, expect)
    }
}
