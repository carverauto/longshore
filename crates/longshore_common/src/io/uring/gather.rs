// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Gathered writes on the ring (`Op::FileWriteSpans`): one vectored write
//! over a row of a table of vectors sized at build. The kernel reads the
//! row while the write is in flight, so a row is its write's until that
//! completes.

use super::*;
use crate::io::files::span_at;

/// No row: what a slot holds when its operation is not a gathered write.
pub(super) const NO_ROW: u32 = u32::MAX;

/// The table of vectors: `Limits::gather_ops` rows of
/// `Limits::gather_spans` entries each.
pub(super) struct Gather {
    pub(super) vectors: Box<[iovec]>,
    spans: usize,
    /// Rows no write holds, a stack; as long as there are rows.
    free: Vec<u32>,
}

impl Gather {
    pub(super) fn new(limits: &Limits) -> Gather {
        let (ops, spans) = (limits.gather_ops as usize, limits.gather_spans as usize);
        let none = iovec {
            iov_base: core::ptr::null_mut(),
            iov_len: 0,
        };
        Gather {
            vectors: vec![none; ops * spans].into_boxed_slice(),
            spans,
            free: (0..limits.gather_ops).rev().collect(),
        }
    }

    /// Whether a row is free for one more write.
    pub(super) fn has_row(&self) -> bool {
        !self.free.is_empty()
    }

    /// `row` is free again.
    pub(super) fn release(&mut self, row: u32) {
        self.free.push(row);
    }
}

impl UringIo {
    /// A vectored write of an `Op::FileWriteSpans`, whose list was checked
    /// when it was submitted, when a row was also seen to be free.
    pub(super) fn prep_file_write_spans(&mut self, i: u32, op: Op) -> io_uring_sqe {
        let Op::FileWriteSpans {
            slot,
            list,
            list_off,
            count,
            file_off,
            ..
        } = op
        else {
            unreachable!("only a gathered write is prepared here")
        };
        let row = self.gather.free.pop().unwrap_or(NO_ROW);
        let first = row as usize * self.gather.spans;
        let at = self.pool_base + self.limits.range(list, list_off, count * 12).start;
        for k in 0..count {
            let (buf, off, len) = span_at(&self.bufs, at, k);
            self.gather.vectors[first + k as usize] = iovec {
                iov_base: self.buf_ptr(buf, off, len) as *mut core::ffi::c_void,
                iov_len: len as usize,
            };
        }
        self.slots[i as usize].state = State::FileSlotOp;
        self.slots[i as usize].row = row;
        let mut sqe = self.fixed_file(slot);
        sqe.opcode = IORING_OP_WRITEV;
        sqe.off = file_off;
        sqe.addr = self.gather.vectors[first..].as_ptr() as u64;
        sqe.len = count;
        sqe
    }
}
