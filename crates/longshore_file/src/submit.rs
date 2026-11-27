// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Handing work to the backend: one operation, or a sequence as one chain,
//! and what waits for room, first in first out (contract C1, C4, C7).

use crate::errno::ended;
use crate::plan::{SPAN_BYTES, STATUS_AREA};
use crate::state::{OState, PathKind, Work, NONE};
use crate::{ErrorKind, FileError, Files, Span};
use longshore_common::io::extents::Extents;
use longshore_common::io::{BufId, Busy, FileSlot, Io, Op, Token};

impl Files {
    /// The backend token of record `i`.
    pub(crate) const fn token(&self, i: u32) -> Token {
        Token::new(self.config.token_base + i as u64)
    }

    /// The record a token is of, if it is the set's.
    pub(crate) fn record_of(&self, t: Token) -> Option<u32> {
        let k = t.get().checked_sub(self.config.token_base)?;
        if k < self.recs.len() as u64 {
            Some(k as u32)
        } else {
            None
        }
    }

    /// Whether record `i` may not be submitted: a write, sync, reserve or
    /// punch of a Poisoned file (contract C10, F6).
    pub(crate) fn blocked(&self, i: u32) -> bool {
        let r = self.recs[i as usize];
        i < self.ops()
            && r.file != NONE
            && self.files[r.file as usize].poisoned
            && !r.work.is_read()
    }

    /// What a blocked record ends with.
    pub(crate) fn blocked_error(&self, i: u32) -> FileError {
        ended(self.recs[i as usize].done, ErrorKind::Poisoned)
    }

    /// The operation for what record `i` has left to do.
    fn op_of<IO: Io>(&mut self, io: &mut IO, i: u32) -> Op {
        let r = self.recs[i as usize];
        let slot = FileSlot::new(self.config.file_slot_base.wrapping_add(r.file));
        let done = r.done as u32;
        match r.work {
            Work::Open { flags, len } => Op::FileOpen {
                slot,
                path_buf: self.file_path_buf(r.file),
                path_off: 0,
                path_len: len,
                flags,
            },
            Work::Close => Op::FileClose { slot },
            Work::Read {
                extent,
                at,
                len,
                file_off,
            } => rw_op(false, slot, (extent.buf(), at, len, file_off), r.done),
            Work::Write {
                extent,
                at,
                len,
                file_off,
            } => rw_op(true, slot, (extent.buf(), at, len, file_off), r.done),
            Work::Spans {
                count,
                len,
                file_off,
            } => Op::FileWriteSpans {
                slot,
                list: self.list_buf(i),
                list_off: 0,
                count: self.write_list(io, i, count),
                len: len - done,
                file_off: file_off + r.done,
            },
            Work::Sync { data_only } => Op::FileSync {
                slot,
                datasync: data_only,
            },
            Work::Reserve {
                off,
                len,
                keep_size,
            } => Op::FileReserve {
                slot,
                file_off: off,
                len,
                keep_size,
            },
            Work::Punch { off, len } => Op::FilePunch {
                slot,
                file_off: off,
                len,
            },
            Work::Path { kind, from, to } => self.path_op(i, kind, (from, to)),
        }
    }

    /// The operation of path record `i`: its paths lie in its own buffer,
    /// behind the status area.
    fn path_op(&self, i: u32, kind: PathKind, (from, to): (u32, u32)) -> Op {
        let buf = self.small(i);
        match kind {
            PathKind::Status => Op::FileStatus {
                path_buf: buf,
                path_off: STATUS_AREA,
                path_len: from,
                out_buf: buf,
                out_off: 0,
            },
            PathKind::Rename => Op::FileRename {
                old_buf: buf,
                old_off: STATUS_AREA,
                old_len: from,
                new_buf: buf,
                new_off: STATUS_AREA + self.config.path_max + 1,
                new_len: to,
            },
            PathKind::Unlink => Op::FileUnlink {
                path_buf: buf,
                path_off: STATUS_AREA,
                path_len: from,
            },
            PathKind::Mkdir => Op::FileMkdir {
                path_buf: buf,
                path_off: STATUS_AREA,
                path_len: from,
            },
        }
    }

    /// Writes record `i`'s list of spans into its list buffer, leaving out
    /// the bytes already written; how many spans it has.
    fn write_list<IO: Io>(&self, io: &mut IO, i: u32, count: u32) -> u32 {
        let row = i as usize * self.config.sizes.spans_max as usize;
        let mut skip = self.recs[i as usize].done;
        let list = io.buf(self.list_buf(i));
        let mut n = 0usize;
        for s in &self.spans[row..row + count as usize] {
            let (buf, off, len) = match *s {
                Span::Extent { extent, at, len } => (extent.buf(), at, len),
                Span::Page { page, off, len } => (page, off, len),
            };
            if skip > 0 && skip >= u64::from(len) {
                skip -= u64::from(len);
                continue;
            }
            let cut = skip as u32;
            skip = 0;
            let at = n * SPAN_BYTES as usize;
            list[at..at + 4].copy_from_slice(&buf.get().to_le_bytes());
            list[at + 4..at + 8].copy_from_slice(&(off + cut).to_le_bytes());
            list[at + 8..at + 12].copy_from_slice(&(len - cut).to_le_bytes());
            n += 1;
        }
        n as u32
    }

    /// Submits record `i` now, or queues it behind what waits.
    pub(crate) fn launch<IO: Io>(&mut self, io: &mut IO, i: u32) {
        if !self.waiting.is_empty() || self.submit_one(io, i).is_err() {
            self.recs[i as usize].state = OState::Waiting;
            self.waiting.push_back(i);
        }
    }

    /// Gives record `i` to the backend.
    fn submit_one<IO: Io>(&mut self, io: &mut IO, i: u32) -> Result<(), Busy> {
        let op = self.op_of(io, i);
        io.submit(self.token(i), op)?;
        self.recs[i as usize].state = OState::Submitted;
        Ok(())
    }

    /// Submits the sequence led by `lead` as one chain now, or queues it.
    pub(crate) fn launch_chain<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, lead: u32) {
        if self.waiting.is_empty() && self.submit_chain(io, x, lead).is_ok() {
            return;
        }
        let mut r = lead;
        while r != NONE {
            self.recs[r as usize].state = OState::Waiting;
            r = self.recs[r as usize].next;
        }
        self.waiting.push_back(lead);
    }

    /// Gives the steps from `lead` on to the backend as one chain, up to
    /// and with the first path step. A step of a Poisoned file that it may
    /// not run ends there, and the steps after it are cancelled; those
    /// before it go.
    fn submit_chain<IO: Io>(
        &mut self,
        io: &mut IO,
        x: &mut Extents,
        lead: u32,
    ) -> Result<(), Busy> {
        let mut n = 0;
        let (mut r, mut last) = (lead, NONE);
        while r != NONE {
            if self.blocked(r) {
                if last != NONE {
                    self.recs[last as usize].next = NONE;
                }
                let next = self.recs[r as usize].next;
                let e = self.blocked_error(r);
                self.finish(io, x, r, Err(e));
                self.cancel_from(io, x, next);
                break;
            }
            let op = self.op_of(io, r);
            self.chain[n] = (self.token(r), op);
            n += 1;
            (last, r) = (r, self.recs[r as usize].next);
            if matches!(self.recs[last as usize].work, Work::Path { .. }) {
                // A chain ends with a path step: a kernel need not end its
                // link when one fails, so what follows goes only once it
                // has reported success.
                break;
            }
        }
        if n == 0 {
            return Ok(());
        }
        io.submit_chain(&self.chain[..n])?;
        let mut k = lead;
        for _ in 0..n {
            self.recs[k as usize].state = OState::Submitted;
            k = self.recs[k as usize].next;
        }
        let l = &mut self.recs[lead as usize];
        (l.out, l.sent) = (n as u32, n as u32);
        Ok(())
    }

    /// Ends every step from `r` on as `Cancelled`.
    pub(crate) fn cancel_from<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, mut r: u32) {
        while r != NONE {
            let next = self.recs[r as usize].next;
            self.finish(io, x, r, Err(FileError::Cancelled));
            r = next;
        }
    }

    /// Submits what waits, first in first out, until the backend is full.
    pub(crate) fn submit_waiting<IO: Io>(&mut self, io: &mut IO, x: &mut Extents) {
        while let Some(i) = self.waiting.pop_front() {
            let r = self.recs[i as usize];
            let queued = r.state == OState::Waiting;
            debug_assert!(queued, "a record queued but not waiting");
            let sent = if !queued {
                Ok(())
            } else if r.lead == i {
                self.submit_chain(io, x, i)
            } else if self.blocked(i) {
                let e = self.blocked_error(i);
                self.finish(io, x, i, Err(e));
                Ok(())
            } else {
                self.submit_one(io, i)
            };
            if sent.is_err() {
                self.waiting.push_front(i);
                return;
            }
        }
    }
}

/// A read or write of `len` bytes of `buf` from `at` at `file_off`, less
/// the `done` bytes already moved.
fn rw_op(
    write: bool,
    slot: FileSlot,
    (buf, at, len, file_off): (BufId, u32, u32, u64),
    done: u64,
) -> Op {
    let (off, rest, file_off) = (at + done as u32, len - done as u32, file_off + done);
    if write {
        Op::FileWrite {
            slot,
            buf,
            off,
            len: rest,
            file_off,
        }
    } else {
        Op::FileRead {
            slot,
            buf,
            off,
            len: rest,
            file_off,
        }
    }
}
