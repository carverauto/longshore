// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a caller's thread and one I/O thread share: operation cells, the
//! pools of operation slots and lent buffers, and the request queue. All of
//! it is allocated when the lane is built.

use super::exact::Budget;
use super::sync::{AtomicBool, AtomicU32, AtomicU64};
use crate::multipart::UploadId;
use crate::report::{Expect, Put};
use crate::s3::{Checksum, Outcome, SubmitError};
use longshore_mailbox::{Pool, Sender};
use std::sync::atomic::Ordering;

/// The longest key a request message carries.
pub const KEY_MAX: usize = 1024;

/// How an operation ended.
#[expect(
    clippy::large_enum_variant,
    reason = "held in a preallocated cell and passed by value: boxing the large variant would allocate on every operation"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Finished {
    Outcome(Outcome),
    Rejected(SubmitError),
    /// The client shut down before the operation ran.
    Closed,
}

/// What a caller asks an I/O thread to do. Fixed size, so the queue that
/// carries it is allocated once.
#[derive(Clone, Copy)]
pub(super) struct Request {
    pub op: u32,
    pub kind: Kind,
    pub key: [u8; KEY_MAX],
    pub key_len: u16,
    /// The lent buffer the operation uses, as an index into the lane's.
    pub buf: u32,
    pub len: u32,
    /// The multipart upload the request belongs to, and its part number.
    pub upload: Option<UploadId>,
    pub number: u32,
    /// For `head` and `get`: the object that must be found.
    pub expect: Expect,
    /// For `put`, `head` and `get`: how far the operation may go.
    pub budget: Budget,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Store {
        no_clobber: bool,
        checksum: Checksum,
    },
    Fetch {
        range: Option<(u64, u64)>,
    },
    Erase,
    /// Begin a multipart upload, asking the store to keep a checksum of
    /// the whole object or not.
    Begin {
        checksums: bool,
    },
    Part {
        checksum: Checksum,
    },
    /// The lent buffer holds the list of parts.
    Complete,
    Abort,
    /// The three that end with a report of what the store answered.
    Put {
        options: Put,
    },
    Head,
    Get {
        range: Option<(u64, u64)>,
    },
}

/// One I/O thread as its callers see it.
pub struct Lane {
    pub(super) sender: Sender<Request, Finished>,
    pub(super) bufs: Pool,
    /// Start of the lane's first lendable buffer, in the I/O thread's pool.
    pub(super) base: *mut u8,
    pub(super) buf_size: usize,
    /// Operations sent and not yet ended, for choosing the least busy lane.
    pub(super) in_flight: AtomicU32,
    /// Credentials adopted from the file, and reloads that failed, as the
    /// I/O thread's store counts them.
    pub(super) reloads: [AtomicU64; 2],
    /// The kernel may still use this lane's buffers: none is lent again or
    /// read or written through a [`super::Buffer`].
    pub(super) void: AtomicBool,
    /// A failure a test has asked this lane's I/O thread to cause in its
    /// ring: 0 for none. See [`super::Client::inject_fault`].
    #[cfg(all(feature = "fault-injection", target_os = "linux"))]
    pub(super) fault: std::sync::atomic::AtomicU8,
}

// The raw pointer is to memory the I/O thread owns for as long as the lane
// lives. A buffer is touched by the caller only while it holds the buffer's
// index, and by the I/O thread only while an operation holds it.
unsafe impl Send for Lane {}
unsafe impl Sync for Lane {}

impl Lane {
    /// Return a lent buffer to the pool, unless the lane is void.
    pub(super) fn give_buffer(&self, index: u32) {
        if !self.is_void() {
            self.bufs.give(index);
        }
    }

    pub(super) fn is_void(&self) -> bool {
        self.void.load(Ordering::Acquire)
    }

    pub(super) fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::Relaxed)
    }
}
