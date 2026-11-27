// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Reserve and punch, and a write from an extent let go of.

use super::ops::Need;
use super::run::Run;
use super::state::{Room, What};
use super::world::BLOCK;
use longshore_file::FileError;

impl Run {
    /// Reserve when `punch` is false, else punch.
    pub fn space(&mut self, punch: bool) {
        let Some(f) = self.pick(true) else {
            return self.turn();
        };
        let (inode, _, _) = self.of(f);
        let (mut why, room) = self.refusals(f, Need::Write);
        let block = u64::from(BLOCK);
        let (mut off, mut len) = if punch {
            (block * self.below(6), block * (1 + self.below(2)))
        } else {
            (self.below(24 * 1024), 1 + self.below(8 * 1024))
        };
        if self.chance(20_000) {
            len = 0;
            why.push(FileError::TooLarge);
        } else if punch && self.chance(40_000) {
            off += 100;
            why.push(FileError::Unaligned);
        }
        let keep = self.chance(500_000);
        self.maybe_place(why.is_empty() && room == Room::Must);
        let t = if punch {
            self.ask("punch", &why, room, |s, io, _| s.punch(io, f, (off, len)))
        } else {
            self.ask("reserve", &why, room, |s, io, _| {
                s.reserve(io, f, (off, len), keep)
            })
        };
        if let (Some(t), Some(i)) = (t, inode) {
            self.begin_change(i, off, off + len, None);
            let w = if punch {
                What::Punch { inode: i, off, len }
            } else {
                What::Reserve {
                    inode: i,
                    off,
                    len,
                    keep,
                }
            };
            self.pend(t, Some(f), w, Vec::new());
        }
    }

    /// A write from an extent the caller already let go of: `StaleExtent`.
    pub fn stale_extent(&mut self) {
        let Some(f) = self.pick(true) else {
            return self.turn();
        };
        let Some(e) = self.take() else {
            return self.turn();
        };
        self.release(e);
        let (mut why, room) = self.refusals(f, Need::Write);
        why.push(FileError::StaleExtent);
        let t = self.ask("a stale extent", &why, room, |s, io, x| {
            s.write_at(io, x, f, (e, 0, 1), 0)
        });
        assert!(t.is_none());
    }
}
