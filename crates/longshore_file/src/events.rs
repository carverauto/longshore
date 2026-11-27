// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Events taken, which free their slots (contract C4); power loss
//! (C12); what the set holds.

use crate::state::{FState, FileRec, OState, OpRec, Work, NONE};
use crate::{Event, File, FileUsage, Files, Ticket};
use longshore_common::io::extents::Extents;

impl Files {
    /// The next event; its record is Free from here.
    pub(crate) fn take_event(&mut self) -> Option<Event> {
        let i = self.events.pop_front()?;
        let r = self.recs[i as usize];
        debug_assert!(
            r.state == OState::Reported,
            "an event of a record not reported"
        );
        let handle = File {
            slot: r.file,
            generation: r.file_gen,
        };
        let opened = !matches!(r.work, Work::Open { .. }) || r.result.is_ok();
        let file = (r.file != NONE && opened).then_some(handle);
        let event = Event::Done {
            ticket: Ticket {
                index: i,
                generation: r.generation,
            },
            file,
            result: r.result,
        };
        self.recs[i as usize] = OpRec {
            generation: r.generation.wrapping_add(1),
            ..OpRec::FREE
        };
        let s = self.config.sizes;
        if i >= self.ops() {
            if self.files[r.file as usize].state == FState::Free {
                self.open_free.push(r.file);
            }
            return Some(event);
        }
        if r.file != NONE {
            self.files[r.file as usize].in_use -= 1;
        }
        if i >= s.files + s.paths {
            self.shared_free.push(i);
        } else if i >= s.files {
            self.path_free.push(i);
        }
        Some(event)
    }

    /// Frees every slot, raises every generation and releases every hold;
    /// what was in flight or not yet taken reports nothing.
    pub(crate) fn reset(&mut self, x: &mut Extents) {
        for i in 0..self.recs.len() as u32 {
            let r = self.recs[i as usize];
            match r.state {
                OState::Waiting | OState::Submitted | OState::Resuming | OState::Held => {
                    self.release_holds(x, i);
                }
                OState::Free | OState::Reported => {}
            }
            self.recs[i as usize] = OpRec {
                generation: r.generation.wrapping_add(1),
                ..OpRec::FREE
            };
        }
        for f in self.files.iter_mut() {
            *f = FileRec {
                generation: f.generation.wrapping_add(1),
                ..FileRec::FREE
            };
        }
        self.waiting.clear();
        self.events.clear();
        let s = self.config.sizes;
        self.shared_free.refill(s.files + s.paths, self.ops());
        self.path_free.refill(s.files, s.files + s.paths);
        self.open_free.refill(0, s.files);
    }

    /// What the set holds now.
    pub(crate) fn count_usage(&self) -> FileUsage {
        let files = self
            .files
            .iter()
            .filter(|f| f.state != FState::Free)
            .count();
        let ops = self.recs.iter().filter(|r| r.state != OState::Free).count();
        let waiting = self
            .recs
            .iter()
            .filter(|r| r.state == OState::Waiting)
            .count();
        FileUsage {
            files: files as u32,
            ops: ops as u32,
            waiting: waiting as u32,
            broken_holds: self.broken_holds,
        }
    }
}
