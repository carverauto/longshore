// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The set's tables: file slots, operation records and the fixed rings
//! that queue them (contract C3, C4).

use crate::FileError;
use longshore_common::io::extents::Extent;
use longshore_common::io::OpenFlags;

/// No record, file or step.
pub(crate) const NONE: u32 = u32::MAX;

/// The state of a file slot (contract C3). Poisoned is `Open` with
/// [`FileRec::poisoned`] set, so that a sync failing while the file closes
/// still poisons what remains of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FState {
    Free,
    Opening,
    Open,
    Closing,
}

/// A file slot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FileRec {
    pub(crate) state: FState,
    pub(crate) generation: u32,
    pub(crate) flags: OpenFlags,
    pub(crate) poisoned: bool,
    /// Operation slots of this file slot not Free: its reserved one and the
    /// shared ones it holds, reported or not.
    pub(crate) in_use: u32,
    /// Its operations not yet reported; its close waits for none.
    pub(crate) active: u32,
}

impl FileRec {
    pub(crate) const FREE: FileRec = FileRec {
        state: FState::Free,
        generation: 0,
        flags: OpenFlags::NONE,
        poisoned: false,
        in_use: 0,
        active: 0,
    };
}

/// The state of an operation record (contract C4). `Held` is a close held
/// back until the file's other operations have reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OState {
    Free,
    Waiting,
    Submitted,
    Resuming,
    Held,
    Reported,
}

/// What an operation does, with what it needs to be submitted again.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Work {
    /// The path is in the file slot's own buffer, `len` bytes with its NUL.
    Open {
        flags: OpenFlags,
        len: u32,
    },
    Close,
    Read {
        extent: Extent,
        at: u32,
        len: u32,
        file_off: u64,
    },
    Write {
        extent: Extent,
        at: u32,
        len: u32,
        file_off: u64,
    },
    /// A gathered write: its spans are the record's row of the span table.
    Spans {
        count: u32,
        len: u32,
        file_off: u64,
    },
    Sync {
        data_only: bool,
    },
    Reserve {
        off: u64,
        len: u64,
        keep_size: bool,
    },
    Punch {
        off: u64,
        len: u64,
    },
    /// A path operation: the lengths of its paths, NUL included; `to` is
    /// 0 but for a rename.
    Path {
        kind: PathKind,
        from: u32,
        to: u32,
    },
}

/// Which path operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PathKind {
    Status,
    Rename,
    Unlink,
    Mkdir,
}

impl Work {
    /// What a Poisoned file still runs: reads (and its close, apart).
    pub(crate) const fn is_read(&self) -> bool {
        matches!(self, Work::Read { .. })
    }

    /// A write or gathered write.
    pub(crate) const fn is_write(&self) -> bool {
        matches!(self, Work::Write { .. } | Work::Spans { .. })
    }

    /// The bytes a transfer moves in all; 0 for the rest.
    pub(crate) const fn len(&self) -> u64 {
        match *self {
            Work::Read { len, .. } | Work::Write { len, .. } | Work::Spans { len, .. } => {
                len as u64
            }
            Work::Open { .. }
            | Work::Close
            | Work::Sync { .. }
            | Work::Reserve { .. }
            | Work::Punch { .. }
            | Work::Path { .. } => 0,
        }
    }
}

/// An operation record: an operation slot, or a file slot's record for its
/// open and close.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpRec {
    pub(crate) state: OState,
    pub(crate) generation: u32,
    /// The file slot, or [`NONE`] for a path operation.
    pub(crate) file: u32,
    /// The file's generation when the operation was accepted.
    pub(crate) file_gen: u32,
    pub(crate) work: Work,
    /// Bytes transferred so far.
    pub(crate) done: u64,
    /// Resumes in a row of a direct write that wrote no whole block.
    pub(crate) stalls: u8,
    pub(crate) result: Result<u64, FileError>,
    /// A sequence step's completion, kept until every step of its chain has
    /// completed.
    pub(crate) raw: i32,
    /// The step that leads the sequence's chain now, or [`NONE`].
    pub(crate) lead: u32,
    /// The next step of the sequence, or [`NONE`].
    pub(crate) next: u32,
    /// At a lead: steps of its chain still at the backend.
    pub(crate) out: u32,
    /// At a lead: steps of the sequence its chain holds; the steps after
    /// them wait for the chain to be read.
    pub(crate) sent: u32,
}

impl OpRec {
    pub(crate) const FREE: OpRec = OpRec {
        state: OState::Free,
        generation: 0,
        file: NONE,
        file_gen: 0,
        work: Work::Close,
        done: 0,
        stalls: 0,
        result: Ok(0),
        raw: 0,
        lead: NONE,
        next: NONE,
        out: 0,
        sent: 0,
    };
}

/// A ring of record indexes of a capacity fixed at build.
pub(crate) struct Ring {
    items: Box<[u32]>,
    head: usize,
    len: usize,
}

impl Ring {
    pub(crate) fn new(capacity: usize) -> Ring {
        Ring {
            items: vec![NONE; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn clear(&mut self) {
        (self.head, self.len) = (0, 0);
    }

    pub(crate) fn push_back(&mut self, i: u32) {
        debug_assert!(self.len < self.items.len(), "a ring overflowed its plan");
        if self.len < self.items.len() {
            let at = (self.head + self.len) % self.items.len();
            self.items[at] = i;
            self.len += 1;
        }
    }

    pub(crate) fn push_front(&mut self, i: u32) {
        debug_assert!(self.len < self.items.len(), "a ring overflowed its plan");
        if self.len < self.items.len() {
            self.head = (self.head + self.items.len() - 1) % self.items.len();
            self.items[self.head] = i;
            self.len += 1;
        }
    }

    pub(crate) fn pop_front(&mut self) -> Option<u32> {
        if self.len == 0 {
            return None;
        }
        let i = self.items[self.head];
        self.head = (self.head + 1) % self.items.len();
        self.len -= 1;
        Some(i)
    }

    /// Keeps, in order, only the items `keep` says yes to.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(u32) -> bool) {
        let n = self.items.len();
        let mut kept = 0;
        for k in 0..self.len {
            let i = self.items[(self.head + k) % n];
            if keep(i) {
                self.items[(self.head + kept) % n] = i;
                kept += 1;
            }
        }
        self.len = kept;
    }
}

/// A stack of free indexes of a capacity fixed at build.
pub(crate) struct Stack {
    items: Box<[u32]>,
    len: usize,
}

impl Stack {
    /// Holding `from..to`, lowest on top.
    pub(crate) fn full(from: u32, to: u32) -> Stack {
        let items: Box<[u32]> = (from..to).rev().collect();
        let len = items.len();
        Stack { items, len }
    }

    pub(crate) fn refill(&mut self, from: u32, to: u32) {
        self.len = 0;
        for i in (from..to).rev() {
            self.push(i);
        }
    }

    pub(crate) fn push(&mut self, i: u32) {
        debug_assert!(self.len < self.items.len(), "a stack overflowed its plan");
        if self.len < self.items.len() {
            self.items[self.len] = i;
            self.len += 1;
        }
    }

    pub(crate) fn pop(&mut self) -> Option<u32> {
        let top = self.len.checked_sub(1)?;
        self.len = top;
        Some(self.items[top])
    }
}
