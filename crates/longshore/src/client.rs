// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A client that callers on any thread can share.
//!
//! A [`Client`] is a handle to one or more I/O threads. Each I/O thread is a
//! complete single-threaded store with its own ring, connections and
//! buffers, and the threads share nothing with each other. One thread is
//! enough to fill a fast network card with plain uploads. More threads are
//! for work that one core cannot carry, such as HTTPS and downloads.
//!
//! Bodies are not copied. A caller borrows a registered [`Buffer`] from the
//! client, fills it, and hands it to an operation, which gives it back with
//! the result. When no buffer or operation slot is free a call waits, so
//! nothing queues without bound.
//!
//! Every operation returns a [`Pending`], which is a `Future` that works
//! under any executor and also has a blocking [`Pending::wait`].
//!
//! ```no_run
//! use longshore::client::Builder;
//! use longshore::store::creds::Credentials;
//!
//! let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
//! let mut settings = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials);
//! settings.tls = true;
//! settings.port = 443;
//! settings.threads = 4; // four independent I/O threads
//! let client = settings.build().unwrap();
//!
//! let mut body = client.buffer_blocking();
//! body.as_mut_slice()[..5].copy_from_slice(b"hello");
//! let (stored, body) = client.store("greeting", body, 5).wait();
//! stored.unwrap();
//! let (fetched, body) = client.fetch("greeting", body).wait();
//! assert_eq!(fetched.unwrap(), Some(5));
//! assert_eq!(&body.as_slice()[..5], b"hello");
//! ```
//!
//! [`Worker`] is one I/O thread's logic with no thread around it, for a
//! caller that drives the I/O itself: the simulator in tests, or an event
//! loop that already owns the core.

mod buffer;
mod lane;
mod pending;
mod threads;
mod worker;

pub use buffer::Buffer;
pub use lane::{Lane, KEY_MAX};
pub use pending::{Output, Pending};
pub use threads::{BuildError, Builder};
pub use worker::{Layout, Worker};

use crate::store::s3::{Checksum, Fatal, Outcome, SubmitError};
use buffer::Borrow;
use lane::{Finished, Kind, Request};
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Why an operation did not do what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The key is empty, or longer than [`KEY_MAX`] or than the store allows.
    BadKey,
    /// The byte range ends before it starts.
    BadRange,
    /// More bytes than the buffer holds.
    TooLong,
    /// The store refused or could not be reached. Retries have been made.
    Store(Fatal),
    /// The operation did not run: the client was shut down, or the I/O
    /// thread it was given to failed. An operation without a buffer goes to
    /// an I/O thread still running when it is sent, but one can fail just
    /// after that, and an operation with a buffer stays on the buffer's
    /// thread. Asking again may succeed on another thread. If the failed
    /// thread's kernel work could not be stopped, a buffer that comes back
    /// with this error is void: see [`Buffer`].
    Closed,
    /// The store answered with something this operation does not expect.
    Unexpected,
}

/// What keeps a threaded client's I/O threads running. `None` for a client
/// over workers the caller drives.
type Alive = Option<Arc<threads::Threads>>;

/// A handle to the I/O threads. Cheap to clone, and usable from any thread.
#[derive(Clone)]
pub struct Client {
    lanes: Arc<[Arc<Lane>]>,
    /// Stops and joins the I/O threads when the last handle, buffer and
    /// operation are gone.
    alive: Alive,
    /// Whether a store computes and sends a checksum of its body.
    checksum: Checksum,
}

impl Client {
    /// A client over lanes whose workers the caller drives. See [`Worker`].
    ///
    /// # Safety
    ///
    /// The lent buffers are memory of the `Io` each worker was built over.
    /// Every such `Io` must outlive this client, its clones, and every
    /// [`Buffer`] and [`Pending`] that came from them.
    pub unsafe fn over(lanes: Vec<Arc<Lane>>) -> Client {
        assert!(!lanes.is_empty(), "a client needs at least one lane");
        Client {
            lanes: lanes.into(),
            alive: None,
            checksum: Checksum::Compute,
        }
    }

    /// A handle that sends, or does not send, a CRC-64/NVME with every
    /// plain stored body. Conditional stores always send one. See
    /// [`Builder::checksums`]. Other handles to the same client are not
    /// changed.
    pub fn with_checksums(mut self, on: bool) -> Client {
        self.checksum = if on {
            Checksum::Compute
        } else {
            Checksum::None
        };
        self
    }

    /// Credentials reloads so far, summed over the I/O threads: how many
    /// times new credentials from the file were adopted, and how many times
    /// a reload failed (the file could not be read or parsed, or the store
    /// refused the request that tries the new credentials). A count of
    /// failures that keeps growing means the file's credentials are not
    /// being taken up. See [`Builder::credentials_validate`].
    pub fn credential_reloads(&self) -> (u64, u64) {
        let sum = |k: usize| {
            self.lanes
                .iter()
                .map(|l| l.reloads[k].load(Ordering::Relaxed))
                .sum()
        };
        (sum(0), sum(1))
    }

    /// How many I/O threads this client has.
    pub fn threads(&self) -> usize {
        self.lanes.len()
    }

    /// The size of every buffer this client lends.
    pub fn buffer_size(&self) -> usize {
        self.lanes[0].buf_size
    }

    /// Borrow a buffer, waiting until one is free on any I/O thread. The
    /// buffer belongs to one I/O thread, and an operation that uses it runs
    /// on that thread.
    pub fn buffer(&self) -> impl Future<Output = Buffer> + '_ {
        Borrow::new(self.lanes.clone(), self.alive.clone())
    }

    /// Borrow a buffer, blocking the calling thread until one is free.
    pub fn buffer_blocking(&self) -> Buffer {
        lane::block_on(self.buffer())
    }

    /// Store the first `len` bytes of `body` under `key`, replacing any
    /// object there. The buffer comes back with the result.
    pub fn store(
        &self,
        key: &str,
        body: Buffer,
        len: usize,
    ) -> Pending<(Result<(), Error>, Buffer)> {
        self.store_as(key, body, len, false, |f| match f {
            Finished::Outcome(Outcome::Stored) => Ok(()),
            other => Err(error_of(other)),
        })
    }

    /// Store only if no object exists under `key`. `Ok(true)` if this call
    /// stored it, `Ok(false)` if an object was already there.
    pub fn store_no_clobber(
        &self,
        key: &str,
        body: Buffer,
        len: usize,
    ) -> Pending<(Result<bool, Error>, Buffer)> {
        self.store_as(key, body, len, true, |f| match f {
            Finished::Outcome(Outcome::Stored) => Ok(true),
            Finished::Outcome(Outcome::AlreadyExists { .. }) => Ok(false),
            other => Err(error_of(other)),
        })
    }

    fn store_as<T: 'static>(
        &self,
        key: &str,
        body: Buffer,
        len: usize,
        no_clobber: bool,
        map: fn(Finished) -> Result<T, Error>,
    ) -> Pending<(Result<T, Error>, Buffer)> {
        let too_long = len > body.lane.buf_size;
        let (lane, buf, alive) = body.into_parts();
        let kind = Kind::Store {
            no_clobber,
            checksum: if no_clobber {
                Checksum::Compute
            } else {
                self.checksum
            },
        };
        let early = too_long.then_some(Error::TooLong);
        Pending::new((lane, alive), key, kind, Some(buf), len as u32, early, map)
    }

    /// Fetch the object under `key` into `into`. `Ok(Some(n))` is its
    /// length, `Ok(None)` means there is no such object.
    pub fn fetch(
        &self,
        key: &str,
        into: Buffer,
    ) -> Pending<(Result<Option<usize>, Error>, Buffer)> {
        self.fetch_as(key, None, into)
    }

    /// Fetch bytes `first..=last` of the object under `key`.
    pub fn fetch_range(
        &self,
        key: &str,
        first: u64,
        last: u64,
        into: Buffer,
    ) -> Pending<(Result<Option<usize>, Error>, Buffer)> {
        self.fetch_as(key, Some((first, last)), into)
    }

    fn fetch_as(
        &self,
        key: &str,
        range: Option<(u64, u64)>,
        into: Buffer,
    ) -> Pending<(Result<Option<usize>, Error>, Buffer)> {
        let (lane, buf, alive) = into.into_parts();
        Pending::new(
            (lane, alive),
            key,
            Kind::Fetch { range },
            Some(buf),
            0,
            None,
            |f| match f {
                Finished::Outcome(Outcome::Fetched { len, .. }) => Ok(Some(len as usize)),
                Finished::Outcome(Outcome::NotFound) => Ok(None),
                other => Err(error_of(other)),
            },
        )
    }

    /// Erase the object under `key`. Erasing a key that is not there succeeds.
    pub fn erase(&self, key: &str) -> Pending<Result<(), Error>> {
        Pending::new(
            (
                self.lanes[least_busy(&self.lanes)].clone(),
                self.alive.clone(),
            ),
            key,
            Kind::Erase,
            None,
            0,
            None,
            |f| match f {
                Finished::Outcome(Outcome::Erased | Outcome::NotFound) => Ok(()),
                other => Err(error_of(other)),
            },
        )
        .on_any(self.lanes.clone())
    }
}

fn error_of(f: Finished) -> Error {
    match f {
        Finished::Outcome(Outcome::Fatal(fatal)) => Error::Store(fatal),
        Finished::Outcome(_) => Error::Unexpected,
        Finished::Rejected(SubmitError::BadKey) => Error::BadKey,
        Finished::Rejected(SubmitError::BadRange) => Error::BadRange,
        Finished::Rejected(_) => Error::Unexpected,
        Finished::Closed => Error::Closed,
    }
}

/// Build the request for `key`, or say why it cannot be sent.
fn request(op: u32, key: &str, kind: Kind, buf: u32, len: u32) -> Result<Request, Error> {
    if key.is_empty() || key.len() > KEY_MAX {
        return Err(Error::BadKey);
    }
    let mut r = Request {
        op,
        kind,
        key: [0; KEY_MAX],
        key_len: key.len() as u16,
        buf,
        len,
    };
    r.key[..key.len()].copy_from_slice(key.as_bytes());
    Ok(r)
}

/// The index of the open lane with the fewest operations in progress, or of
/// a closed one if none is open.
fn least_busy(lanes: &[Arc<Lane>]) -> usize {
    (0..lanes.len())
        .min_by_key(|&n| (!open(&lanes[n]), lanes[n].in_flight()))
        .expect("at least one lane")
}

/// Whether the lane still takes requests.
fn open(lane: &Lane) -> bool {
    !lane.closed.load(Ordering::Acquire)
}
