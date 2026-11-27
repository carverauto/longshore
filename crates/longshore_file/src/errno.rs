// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Backend results made one error, the same on every backend (contract
//! C10). Every backend reports Linux errno values.

use crate::{ErrorKind, FileError};

const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EIO: i32 = 5;
const EBADF: i32 = 9;
const EACCES: i32 = 13;
const EEXIST: i32 = 17;
const EINVAL: i32 = 22;
const ENOSPC: i32 = 28;
const EOPNOTSUPP: i32 = 95;
const EDQUOT: i32 = 122;
/// A step of a chain the backend did not run.
pub(crate) const ECANCELED: i32 = 125;

/// The errno of a failed completion, as a positive number.
pub(crate) const fn errno_of(result: i32) -> i32 {
    result.saturating_neg()
}

/// What a failed operation that moved no byte reports.
pub(crate) const fn error(errno: i32) -> FileError {
    match errno {
        ENOENT => FileError::NotFound,
        EEXIST => FileError::Exists,
        EACCES | EPERM => FileError::Denied,
        ENOSPC | EDQUOT => FileError::NoSpace,
        EOPNOTSUPP => FileError::Unsupported,
        EIO => FileError::Io { errno },
        EINVAL | EBADF => FileError::Invalid { errno },
        _ => FileError::Os { errno },
    }
}

/// The kind inside a `Partial`: as [`error`], with the kinds a transfer
/// cannot name kept as `Os`.
const fn kind(errno: i32) -> ErrorKind {
    match errno {
        ENOSPC | EDQUOT => ErrorKind::NoSpace,
        EOPNOTSUPP => ErrorKind::Unsupported,
        EIO => ErrorKind::Io { errno },
        EINVAL | EBADF => ErrorKind::Invalid { errno },
        _ => ErrorKind::Os { errno },
    }
}

/// What a failed sync reports: `Unsupported`, or `SyncFailed` whatever the
/// errno, which poisons the file.
pub(crate) const fn sync_error(errno: i32) -> FileError {
    match errno {
        EOPNOTSUPP => FileError::Unsupported,
        _ => FileError::SyncFailed { errno },
    }
}

/// A transfer that ended with `kind` after `done` bytes.
pub(crate) const fn ended(done: u64, kind: ErrorKind) -> FileError {
    if done > 0 {
        return FileError::Partial { done, kind };
    }
    match kind {
        ErrorKind::Poisoned => FileError::Poisoned,
        ErrorKind::NoProgress => FileError::NoProgress,
        ErrorKind::NoSpace => FileError::NoSpace,
        ErrorKind::Unsupported => FileError::Unsupported,
        ErrorKind::SyncFailed { errno } => FileError::SyncFailed { errno },
        ErrorKind::Io { errno } => FileError::Io { errno },
        ErrorKind::Invalid { errno } => FileError::Invalid { errno },
        ErrorKind::Os { errno } => error(errno),
    }
}

/// Whether a write to a file opened for durable writes that failed with
/// `errno` failed as a sync does, leaving earlier bytes of unknown state:
/// an error of the medium (`EIO`, which the portable backend also reports
/// for errors it does not know) or no room for the write-back (`ENOSPC`,
/// `EDQUOT`). Any other errno is the write's own refusal.
pub(crate) const fn lost_write(errno: i32) -> bool {
    matches!(errno, EIO | ENOSPC | EDQUOT)
}

/// A transfer that failed with `errno` after `done` bytes; `durable` for
/// a failed write to a file opened for durable writes that
/// [`lost_write`] says is a failed sync.
pub(crate) const fn transfer_error(done: u64, errno: i32, durable: bool) -> FileError {
    if durable {
        return ended(done, ErrorKind::SyncFailed { errno });
    }
    if done == 0 {
        return error(errno);
    }
    ended(done, kind(errno))
}
