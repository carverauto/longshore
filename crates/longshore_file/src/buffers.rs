// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The set's small buffers (paths, status areas, span lists) and its holds
//! on extents (contract C2, C8).

use crate::{FileError, Files, Span};
use longshore_common::io::extents::{Extent, Extents};
use longshore_common::io::{BufId, Io};

impl Files {
    /// The length of `path` with its NUL, if a path buffer holds it.
    pub(crate) fn path_len(&self, path: &str) -> Result<u32, FileError> {
        let b = path.as_bytes();
        if b.is_empty() || b.len() > self.config.path_max as usize || b.contains(&0) {
            return Err(FileError::BadPath);
        }
        Ok(b.len() as u32 + 1)
    }

    /// Copies `path` and its NUL into `buf` from `off`: a path, not payload.
    pub(crate) fn put_path<IO: Io>(&self, io: &mut IO, buf: BufId, off: u32, path: &str) {
        let b = path.as_bytes();
        let at = off as usize;
        let dst = &mut io.buf(buf)[at..=at + b.len()];
        dst[..b.len()].copy_from_slice(b);
        dst[b.len()] = 0;
    }

    /// The set's small buffer `k`: file slots' paths, then path slots',
    /// then operation slots' span lists.
    pub(crate) const fn small(&self, k: u32) -> BufId {
        BufId::new(self.small_first + k)
    }

    /// File slot `f`'s path buffer.
    pub(crate) const fn file_path_buf(&self, f: u32) -> BufId {
        self.small(f)
    }

    /// Operation slot `i`'s span list.
    pub(crate) const fn list_buf(&self, i: u32) -> BufId {
        let s = self.config.sizes;
        self.small(s.files + s.paths + i)
    }

    /// Releases the holds on the first `n` spans of row `row`.
    pub(crate) fn release_spans(&mut self, x: &mut Extents, row: usize, n: usize) {
        for k in 0..n {
            if let Span::Extent { extent, .. } = self.spans[row + k] {
                self.release(x, extent);
            }
        }
    }

    /// Releases one of the set's holds; one that fails was released by
    /// someone else, a broken invariant (contract C8).
    pub(crate) fn release(&mut self, x: &mut Extents, e: Extent) {
        let released = x.release(e);
        debug_assert!(released.is_ok(), "the set's hold was released by another");
        if released.is_err() {
            self.broken_holds = self.broken_holds.saturating_add(1);
        }
    }
}
