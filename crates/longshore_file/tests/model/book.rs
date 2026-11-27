// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the model keeps of each file's bytes, for F7 and F8.
//!
//! For every byte of the first [`SPAN`] bytes of a file: what a read must
//! return (`content`, where `known`), how many writes of it are in flight
//! (`taint`) and whether two of them overlapped in time (`conflicted`,
//! then the byte is unknown once they are done), the value a power loss
//! must keep (`durable`), and the value written whole and not yet synced
//! (`dirty`, with the model's clock at the write's event).

/// The bytes of a file the model follows; the steps stay below it.
pub const SPAN: usize = 32 * 1024;

/// One file of the disk, as the model knows it.
pub struct Inode {
    pub content: Vec<u8>,
    pub known: Vec<bool>,
    pub taint: Vec<u16>,
    pub conflicted: Vec<bool>,
    pub durable: Vec<Option<u8>>,
    pub dirty: Vec<u64>,
    pub dirty_val: Vec<u8>,
    /// Its exact size, while it is known.
    pub size: Option<u64>,
    /// Operations in flight that may change its size.
    pub size_busy: u32,
    /// Opens that empty it, in flight.
    pub emptying: u32,
    /// Another change of it was in flight, or ended, while one was: which
    /// came first at the disk, and so its size, cannot be said.
    pub raced: bool,
}

impl Inode {
    /// A new, empty file: every byte a read can reach is a zero.
    pub fn new() -> Inode {
        Inode {
            content: vec![0; SPAN],
            known: vec![true; SPAN],
            taint: vec![0; SPAN],
            conflicted: vec![false; SPAN],
            durable: vec![None; SPAN],
            dirty: vec![0; SPAN],
            dirty_val: vec![0; SPAN],
            size: Some(0),
            size_busy: 0,
            emptying: 0,
            raced: false,
        }
    }

    /// Emptied by an open: zeros, size 0, nothing promised after a power
    /// loss until it is written and synced again.
    pub fn empty(&mut self) {
        self.content.fill(0);
        self.known.fill(true);
        self.durable.fill(None);
        self.dirty.fill(0);
        self.size = Some(0);
    }

    /// An open that empties it is taken; [`Inode::emptied`] ends it.
    pub fn emptying(&mut self) {
        self.begin(0, SPAN);
        self.emptying += 1;
        self.raced = false;
    }

    /// The open that empties it reported: if nothing else changed it
    /// meanwhile it is empty, else its size is unknown from now on.
    pub fn emptied(&mut self) {
        self.emptying -= 1;
        self.end(0, SPAN, |_| Some(Some(0)), None, false);
        if self.raced {
            self.size = None;
            self.raced = false;
        } else {
            self.empty();
        }
    }

    /// A write, reserve or punch of `[lo, hi)` is accepted: it is in
    /// flight over those bytes, and a power loss now promises nothing of
    /// them.
    pub fn begin(&mut self, lo: usize, hi: usize) {
        self.raced |= self.emptying > 0;
        for b in lo..hi.min(SPAN) {
            if self.taint[b] > 0 {
                self.conflicted[b] = true;
            }
            self.taint[b] += 1;
            self.durable[b] = None;
            self.dirty[b] = 0;
        }
        self.size_busy += 1;
    }

    /// It is over: `put` gives each byte's new value, `None` for unknown,
    /// `Some(None)` for unchanged. `at` is the model's clock, for a write
    /// that completed whole; `durable` when it is durable already.
    pub fn end(
        &mut self,
        lo: usize,
        hi: usize,
        put: impl Fn(usize) -> Option<Option<u8>>,
        whole: Option<u64>,
        durable: bool,
    ) {
        self.raced |= self.emptying > 0;
        for b in lo..hi.min(SPAN) {
            self.taint[b] -= 1;
            match put(b) {
                Some(None) => {}
                Some(Some(v)) if !self.conflicted[b] => {
                    self.content[b] = v;
                    self.known[b] = true;
                    if durable {
                        self.durable[b] = Some(v);
                    } else if let Some(at) = whole {
                        self.dirty[b] = at;
                        self.dirty_val[b] = v;
                    }
                }
                Some(Some(_)) | None => {
                    self.known[b] = false;
                    self.dirty[b] = 0;
                }
            }
            if self.conflicted[b] {
                self.known[b] = false;
                self.dirty[b] = 0;
                if self.taint[b] == 0 {
                    self.conflicted[b] = false;
                }
            }
        }
        self.size_busy -= 1;
    }

    /// A sync accepted at `accepted` succeeded: what was written whole
    /// before it is durable.
    pub fn synced(&mut self, accepted: u64) {
        for b in 0..SPAN {
            if self.dirty[b] != 0 && self.dirty[b] < accepted {
                self.durable[b] = Some(self.dirty_val[b]);
                self.dirty[b] = 0;
            }
        }
    }

    /// A sync of the file failed, or a durable write did: nothing written
    /// before is promised any more (F8: no failed sync after the write).
    pub fn sync_failed(&mut self) {
        self.durable.fill(None);
        self.dirty.fill(0);
    }

    /// How far a power loss must keep bytes: one past the last promised.
    pub fn promised_end(&self) -> usize {
        self.durable
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |b| b + 1)
    }
}
