// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Accepting an operation: the checks before anything is taken (contract
//! C9, C11), then its slot (C2) and its holds (C8). A refusal takes no
//! slot and no hold.

use crate::plan::STATUS_AREA;
use crate::state::{FState, OState, OpRec, PathKind, Work, NONE};
use crate::{File, FileError, Files, Range, Span};
use longshore_common::io::extents::{ExtentError, Extents};
use longshore_common::io::{BufId, Io, OpenFlags};

/// The highest file offset a kernel takes (`loff_t`).
const OFFSET_MOST: u64 = i64::MAX.unsigned_abs();

/// What a refused hold is reported as.
pub(crate) const fn hold_error(e: ExtentError) -> FileError {
    match e {
        ExtentError::Stale => FileError::StaleExtent,
        ExtentError::TooManyHolds => FileError::TooManyHolds,
    }
}

impl Files {
    /// Operation slots: reserved, path and shared, in that order.
    pub(crate) const fn ops(&self) -> u32 {
        let s = self.config.sizes;
        s.files + s.paths + s.shared
    }

    /// The file slot `file` names, if it is Open or Poisoned.
    pub(crate) fn live(&self, file: File) -> Result<u32, FileError> {
        let Some(rec) = self.files.get(file.slot as usize) else {
            return Err(FileError::Stale);
        };
        if rec.generation != file.generation {
            return Err(FileError::Stale);
        }
        match rec.state {
            FState::Open => Ok(file.slot),
            FState::Closing => Err(FileError::Closing),
            FState::Free | FState::Opening => Err(FileError::Stale),
        }
    }

    /// As [`Files::live`], and refused `Poisoned` unless `read`.
    pub(crate) fn usable(&self, file: File, read: bool) -> Result<u32, FileError> {
        let f = self.live(file)?;
        if self.files[f as usize].poisoned && !read {
            return Err(FileError::Poisoned);
        }
        Ok(f)
    }

    /// Whether `v` is whole blocks.
    fn whole(&self, v: u64) -> bool {
        let bs = u64::from(self.limits.block_size());
        bs != 0 && v.is_multiple_of(bs)
    }

    /// `len` bytes of buffer `buf` from `at`: inside it, and for direct
    /// I/O whole blocks from a block boundary of `ok_class`.
    fn check_buf(
        &self,
        buf: BufId,
        (at, len): (u32, u32),
        direct: bool,
        ok_class: bool,
    ) -> Result<(), FileError> {
        if buf.get() >= self.limits.buffer_count() {
            return Err(FileError::TooLarge);
        }
        let (_, size) = self.limits.span(buf);
        if u64::from(at) + u64::from(len) > size as u64 {
            return Err(FileError::TooLarge);
        }
        if direct && !(ok_class && self.whole(u64::from(at)) && self.whole(u64::from(len))) {
            return Err(FileError::Unaligned);
        }
        Ok(())
    }

    /// An extent's range, as [`Files::check_buf`], the extent live.
    fn check_extent(
        &self,
        x: &Extents,
        (e, at, len): Range,
        direct: bool,
    ) -> Result<(), FileError> {
        if !x.is_live(e) {
            return Err(FileError::StaleExtent);
        }
        let aligned = self.limits.is_aligned_buf(e.buf());
        self.check_buf(e.buf(), (at, len), direct, aligned)
    }

    /// A range of the file: inside what a kernel takes, and for direct
    /// I/O from a block boundary.
    fn check_file_range(&self, file_off: u64, len: u64, direct: bool) -> Result<(), FileError> {
        if file_off
            .checked_add(len)
            .is_none_or(|end| end > OFFSET_MOST)
        {
            return Err(FileError::TooLarge);
        }
        if direct && !self.whole(file_off) {
            return Err(FileError::Unaligned);
        }
        Ok(())
    }

    /// An operation slot for file slot `f`: its reserved one, else a
    /// shared one, up to `ops_per_file` at once.
    fn take_op(&mut self, f: u32) -> Result<u32, FileError> {
        let rec = self.files[f as usize];
        if rec.in_use >= self.config.sizes.ops_per_file {
            return Err(FileError::Full);
        }
        let i = if self.recs[f as usize].state == OState::Free {
            f
        } else {
            self.shared_free.pop().ok_or(FileError::Full)?
        };
        let file = &mut self.files[f as usize];
        (file.in_use, file.active) = (file.in_use + 1, file.active + 1);
        let r = &mut self.recs[i as usize];
        (r.state, r.file, r.file_gen) = (OState::Waiting, f, file.generation);
        Ok(i)
    }

    /// Gives back a slot taken for an operation that is refused after all.
    pub(crate) fn untake(&mut self, i: u32) {
        let r = self.recs[i as usize];
        if r.file != NONE {
            let file = &mut self.files[r.file as usize];
            (file.in_use, file.active) = (file.in_use - 1, file.active - 1);
        }
        let s = self.config.sizes;
        if i >= s.files + s.paths {
            self.shared_free.push(i);
        } else if i >= s.files {
            self.path_free.push(i);
        }
        self.recs[i as usize] = OpRec {
            generation: r.generation.wrapping_add(1),
            ..OpRec::FREE
        };
    }

    /// Fills record `i`, taken for file `f` (or a path), with `work`.
    fn begin(&mut self, i: u32, work: Work) {
        let r = &mut self.recs[i as usize];
        *r = OpRec {
            state: OState::Waiting,
            generation: r.generation,
            file: r.file,
            file_gen: r.file_gen,
            work,
            ..OpRec::FREE
        };
    }

    /// A read (`write` false) or write of `range` at `file_off`.
    pub(crate) fn accept_rw(
        &mut self,
        x: &mut Extents,
        file: File,
        range: Range,
        file_off: u64,
        write: bool,
    ) -> Result<u32, FileError> {
        let f = self.usable(file, !write)?;
        let flags = self.files[f as usize].flags;
        if !may(flags, write) {
            return Err(FileError::NotOpen);
        }
        self.check_extent(x, range, flags.direct())?;
        self.check_file_range(file_off, u64::from(range.2), flags.direct())?;
        let i = self.take_op(f)?;
        let (extent, at, len) = range;
        if let Err(e) = x.hold(extent) {
            self.untake(i);
            return Err(hold_error(e));
        }
        let work = if write {
            Work::Write {
                extent,
                at,
                len,
                file_off,
            }
        } else {
            Work::Read {
                extent,
                at,
                len,
                file_off,
            }
        };
        self.begin(i, work);
        Ok(i)
    }

    /// One span of a gathered write, as [`Files::check_buf`].
    fn check_span(&self, x: &Extents, s: Span, direct: bool) -> Result<u32, FileError> {
        match s {
            Span::Extent { extent, at, len } => {
                self.check_extent(x, (extent, at, len), direct)?;
                Ok(len)
            }
            Span::Page { page, off, len } => {
                if !self.limits.is_page(page) {
                    return Err(FileError::TooLarge);
                }
                self.check_buf(page, (off, len), direct, true)?;
                Ok(len)
            }
        }
    }

    /// A gathered write of `spans` at `file_off`.
    pub(crate) fn accept_spans(
        &mut self,
        x: &mut Extents,
        file: File,
        spans: &[Span],
        file_off: u64,
    ) -> Result<u32, FileError> {
        let f = self.usable(file, false)?;
        let flags = self.files[f as usize].flags;
        if !may(flags, true) {
            return Err(FileError::NotOpen);
        }
        let max = self.config.sizes.spans_max as usize;
        if spans.is_empty() || spans.len() > max {
            return Err(FileError::TooLarge);
        }
        let mut sum = 0u32;
        for s in spans {
            let len = self.check_span(x, *s, flags.direct())?;
            sum = sum.checked_add(len).ok_or(FileError::TooLarge)?;
        }
        self.check_file_range(file_off, u64::from(sum), flags.direct())?;
        let i = self.take_op(f)?;
        let row = i as usize * max;
        for (k, s) in spans.iter().enumerate() {
            if let Span::Extent { extent, .. } = *s {
                if let Err(e) = x.hold(extent) {
                    self.release_spans(x, row, k);
                    self.untake(i);
                    return Err(hold_error(e));
                }
            }
            self.spans[row + k] = *s;
        }
        let count = spans.len() as u32;
        let work = Work::Spans {
            count,
            len: sum,
            file_off,
        };
        self.begin(i, work);
        Ok(i)
    }

    /// A sync, reserve or punch of `file`.
    pub(crate) fn accept_file_op(&mut self, file: File, work: Work) -> Result<u32, FileError> {
        let f = self.usable(file, false)?;
        let flags = self.files[f as usize].flags;
        match work {
            Work::Reserve { off, len, .. } | Work::Punch { off, len } => {
                if !flags.write() {
                    return Err(FileError::NotOpen);
                }
                if len == 0 {
                    return Err(FileError::TooLarge);
                }
                self.check_file_range(off, len, false)?;
                let punch = matches!(work, Work::Punch { .. });
                if punch && !(self.whole(off) && self.whole(len)) {
                    return Err(FileError::Unaligned);
                }
            }
            Work::Sync { .. }
            | Work::Open { .. }
            | Work::Close
            | Work::Read { .. }
            | Work::Write { .. }
            | Work::Spans { .. }
            | Work::Path { .. } => {}
        }
        let i = self.take_op(f)?;
        self.begin(i, work);
        Ok(i)
    }

    /// A path operation on `path` (and `to`, for a rename).
    pub(crate) fn accept_path_op<IO: Io>(
        &mut self,
        io: &mut IO,
        kind: PathKind,
        path: &str,
        to: Option<&str>,
    ) -> Result<u32, FileError> {
        let a = self.path_len(path)?;
        let b = match to {
            Some(t) => self.path_len(t)?,
            None => 0,
        };
        let i = self.path_free.pop().ok_or(FileError::Full)?;
        let buf = self.small(i);
        self.put_path(io, buf, STATUS_AREA, path);
        if let Some(t) = to {
            self.put_path(io, buf, STATUS_AREA + self.config.path_max + 1, t);
        }
        let r = &mut self.recs[i as usize];
        (r.state, r.file) = (OState::Waiting, NONE);
        self.begin(
            i,
            Work::Path {
                kind,
                from: a,
                to: b,
            },
        );
        Ok(i)
    }

    /// File slot `f`'s record, filled for its open; `f` was Free.
    pub(crate) fn begin_open(&mut self, f: u32, work: Work, flags: OpenFlags) -> u32 {
        let i = self.ops() + f;
        let file = &mut self.files[f as usize];
        debug_assert!(file.state == FState::Free, "an open of a slot in use");
        (file.state, file.flags, file.poisoned) = (FState::Opening, flags, false);
        let gen = file.generation;
        let r = &mut self.recs[i as usize];
        debug_assert!(r.state == OState::Free, "an open of a slot not yet free");
        (r.file, r.file_gen) = (f, gen);
        self.begin(i, work);
        i
    }

    /// File slot `f`'s record, filled for its close and held back.
    pub(crate) fn begin_close(&mut self, f: u32) -> Result<u32, FileError> {
        let i = self.ops() + f;
        // The open's event names the file, so it has been taken and the
        // file's own record is Free.
        let free = self.recs[i as usize].state == OState::Free;
        debug_assert!(free, "a close while the file's own record is in use");
        if !free {
            return Err(FileError::Full);
        }
        let file = &mut self.files[f as usize];
        file.state = FState::Closing;
        let gen = file.generation;
        let r = &mut self.recs[i as usize];
        (r.file, r.file_gen) = (f, gen);
        self.begin(i, Work::Close);
        self.recs[i as usize].state = OState::Held;
        Ok(i)
    }
}

/// Whether a file opened with `flags` may be written (`write`) or read.
const fn may(flags: OpenFlags, write: bool) -> bool {
    if write {
        flags.write()
    } else {
        flags.read() || !flags.write()
    }
}
