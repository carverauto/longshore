// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a file operation can end with, or be refused with: one type, the
//! same on every backend (contract C10, C11).

/// Why an operation was refused at the call, or how it ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileError {
    /// No operation slot for this file or the set, or no path slot.
    Full,
    /// The handle's file was closed; its slot may hold another.
    Stale,
    /// The file is closing.
    Closing,
    /// A sync of the file failed, or a write of a durable-write file: only
    /// reads and `close` are accepted.
    Poisoned,
    /// The file was not opened for this: a write on a read-only file, or a
    /// read on a write-only one.
    NotOpen,
    /// On a direct-I/O file: a buffer, length or offset not whole blocks.
    Unaligned,
    /// More spans or steps than planned, a range past its extent, a
    /// transfer of more than `i32::MAX` bytes (what a backend's completion
    /// counts), or a length of 0 where one is needed.
    TooLarge,
    /// A path longer than the plan allows, or with a NUL inside.
    BadPath,
    /// An extent freed since it was taken, or of another range.
    StaleExtent,
    /// An extent with as many holds as can be counted.
    TooManyHolds,
    /// A step of a sequence after one that failed.
    Cancelled,
    /// A write that completed with no byte written.
    NoProgress,
    /// A transfer that ended with an error after `done` bytes.
    Partial {
        /// Bytes transferred before the error.
        done: u64,
        /// The error.
        kind: ErrorKind,
    },
    /// `ENOENT`.
    NotFound,
    /// `EEXIST`.
    Exists,
    /// `EACCES`, `EPERM`.
    Denied,
    /// `ENOSPC`, `EDQUOT`.
    NoSpace,
    /// `EOPNOTSUPP`.
    Unsupported,
    /// A sync failed: the file is Poisoned.
    SyncFailed {
        /// The backend's errno, as a positive number.
        errno: i32,
    },
    /// `EIO` outside a sync.
    Io {
        /// The backend's errno, as a positive number.
        errno: i32,
    },
    /// `EINVAL`, `EBADF`: impossible after the set's own checks; a bug.
    Invalid {
        /// The backend's errno, as a positive number.
        errno: i32,
    },
    /// Any other errno.
    Os {
        /// The backend's errno, as a positive number.
        errno: i32,
    },
}

/// The kind of error inside [`FileError::Partial`]: the same as
/// [`FileError`] without `Partial`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// See [`FileError::Poisoned`].
    Poisoned,
    /// See [`FileError::NoProgress`].
    NoProgress,
    /// See [`FileError::NoSpace`].
    NoSpace,
    /// See [`FileError::Unsupported`].
    Unsupported,
    /// See [`FileError::SyncFailed`].
    SyncFailed {
        /// The backend's errno.
        errno: i32,
    },
    /// See [`FileError::Io`].
    Io {
        /// The backend's errno.
        errno: i32,
    },
    /// See [`FileError::Invalid`].
    Invalid {
        /// The backend's errno.
        errno: i32,
    },
    /// See [`FileError::Os`].
    Os {
        /// The backend's errno.
        errno: i32,
    },
}
