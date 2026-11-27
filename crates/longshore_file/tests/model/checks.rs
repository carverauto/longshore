// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The invariants after every step, and at the end of a run: holds (F3),
//! slots and usage (F4, F10), and what the set submitted for each file
//! slot (F1, F6), read from the spied backend.

use super::run::Run;
use super::state::{HState, What};
use super::world::{PATHS, PER_FILE};
use longshore_file::check::Kind;

impl Run {
    pub fn check(&mut self) {
        // F3: every extent an operation in flight names is held by the set.
        for p in &self.st.pending {
            for x in &p.extents {
                let naming = self
                    .st
                    .pending
                    .iter()
                    .filter(|q| q.extents.contains(x))
                    .count();
                let want = usize::from(self.st.ours(*x)) + naming;
                let held = self.rig.x().holds(*x).map_or(0, usize::from);
                assert_eq!(held, want, "F3: holds of {x:?} in flight");
            }
        }
        // F4, and the usage agrees with the tickets in flight.
        let u = self.rig.usage();
        let opening = self
            .st
            .pending
            .iter()
            .filter(|p| matches!(p.what, What::Open { .. }))
            .count();
        assert_eq!(
            u.ops() as usize,
            self.st.pending.len(),
            "operations in use: {u:?}"
        );
        assert_eq!(u.files() as usize, self.st.handles.len() + opening, "{u:?}");
        assert!(u.waiting() <= u.ops() && u.broken_holds() == 0, "{u:?}");
        for h in &self.st.handles {
            assert!(
                self.st.in_flight(h.file) <= PER_FILE,
                "F4: {:?} over its cap",
                h.file
            );
        }
        let paths = self.st.pending.iter().filter(|p| p.of_path()).count() as u32;
        assert!(paths <= PATHS, "F4: {paths} path operations");
    }

    /// F1 and F6, after each turn and before its events are taken:
    /// nothing submitted for a file slot no handle holds but an open, no
    /// open for a slot a handle holds, and nothing but reads and its close
    /// for a file poisoned by an event taken before.
    pub fn submitted_rightly(&mut self) {
        let from = self.mark;
        for s in self.rig.io().since(from) {
            let (kind, slot, _) = s.describe();
            let Some(slot) = slot else { continue };
            let h = self.st.handles.iter().find(|h| h.file.slot() == slot.get());
            match h {
                None => assert!(kind == Kind::Open, "F1: {s:?} for a free slot"),
                Some(h) => {
                    assert!(kind != Kind::Open, "F1: an open of a slot in use: {s:?}");
                    if h.poisoned {
                        let read_or_close = matches!(kind, Kind::Read | Kind::Close);
                        assert!(read_or_close, "F6: {s:?} of a poisoned file");
                    }
                }
            }
        }
        self.mark = self.rig.io().submitted();
    }

    /// The end of a run: settled, every slot agrees (F10); every file
    /// closed; the set holds no extent.
    pub fn finish(&mut self) {
        self.settle();
        self.check();
        let u = self.rig.usage();
        assert_eq!((u.ops(), u.waiting()), (0, 0), "F10: {u:?}");
        assert_eq!(u.files() as usize, self.st.handles.len(), "F10: {u:?}");
        let open: Vec<_> = self.st.handles.iter().map(|h| h.file).collect();
        for f in open {
            let t = self.ask(
                "the last close",
                &[],
                super::state::Room::Must,
                |s, io, _| s.close(io, f),
            );
            let t = t.unwrap();
            if let Some(h) = self.st.handle_mut(f) {
                h.state = HState::Closing;
            }
            self.pend(t, Some(f), What::Close, Vec::new());
        }
        self.settle();
        self.check();
        assert_eq!(self.rig.usage().files(), 0, "F10: every file closed");
        for (e, n) in self.st.ours.clone() {
            for _ in 0..n {
                self.release(e);
            }
        }
        let x = self.rig.x();
        assert_eq!(
            x.free_count(),
            x.count(),
            "F3: the set holds an extent at rest"
        );
    }
}
