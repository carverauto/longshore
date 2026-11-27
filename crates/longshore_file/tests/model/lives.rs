// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Events that make and end handles, and poisoning: an open's handle is of
//! a slot no handle holds, with a generation the slot's last one did not
//! have; a close leaves it stale; a failed sync or durable write poisons
//! it, and an operation that ends `Poisoned` before that failure's own
//! event is held to the end of the turn.

use super::run::Run;
use super::state::{HState, Handle, Target};
use longshore_file::{ErrorKind, FileError};

impl Run {
    pub fn poisoned(&self, f: Option<longshore_file::File>) -> bool {
        f.and_then(|f| self.st.handle(f))
            .is_some_and(|h| h.poisoned)
    }

    /// Whether `f` is poisoned, for judging `result`. An operation that
    /// waited ends `Poisoned` when the failure that poisons its file is
    /// seen, and its event can come before that failure's own: then the
    /// file must be poisoned by the end of the turn.
    pub fn poisoned_for(
        &mut self,
        f: Option<longshore_file::File>,
        result: Result<u64, FileError>,
    ) -> bool {
        let says = matches!(
            result,
            Err(FileError::Poisoned
                | FileError::Partial {
                    kind: ErrorKind::Poisoned,
                    ..
                })
        );
        if self.poisoned(f) {
            return true;
        }
        if let (true, Some(f)) = (says, f) {
            self.poison_due.push(f);
        }
        says
    }

    /// The end of a turn's events: every file an event said was poisoned
    /// is, or is gone.
    pub fn poison_seen(&mut self) {
        for f in core::mem::take(&mut self.poison_due) {
            let h = self.st.handle(f);
            assert!(
                h.is_none_or(|h| h.poisoned),
                "an event said {f:?} was poisoned, and no failure poisoned it"
            );
        }
    }

    /// Poison the handle `f`, as a failed sync or durable write does.
    pub fn poison(&mut self, f: Option<longshore_file::File>) {
        let (step, now) = (self.turns, self.st.clock);
        if let Some(h) = f.and_then(|f| self.st.handle_mut(f)) {
            h.poisoned = true;
            if h.poisoned_at == 0 {
                h.poisoned_at = now;
            }
            if h.state == HState::Open {
                h.state = HState::Poisoned(step);
            }
        }
        self.stats.poisoned += 1;
    }

    /// An open's event: a handle comes, of a file new or not.
    pub fn opened(
        &mut self,
        (target, dir, flags, expect): (
            Option<usize>,
            Option<Target>,
            longshore_common::io::OpenFlags,
            Result<(), FileError>,
        ),
        file: Option<longshore_file::File>,
        result: Result<u64, FileError>,
    ) {
        assert_eq!(result.map(|_| ()), expect, "open");
        let Some(f) = file.filter(|_| result.is_ok()) else {
            return;
        };
        assert!(self.st.handles.iter().all(|h| h.file.slot() != f.slot()));
        let slot = f.slot() as usize;
        assert!(
            self.st.last_gen[slot] != Some(f.generation()),
            "a new generation"
        );
        let target = match (target, dir) {
            (Some(n), _) => {
                let existed = self.st.names[n].binding.is_some();
                let i = self.st.names[n].binding.unwrap_or_else(|| {
                    self.st.inodes.push(super::book::Inode::new());
                    let i = self.st.inodes.len() - 1;
                    let name = &mut self.st.names[n];
                    name.binding = Some(i);
                    let in_sub = name.in_sub;
                    name.change(self.st.clock, [!in_sub, in_sub]);
                    i
                });
                if flags.empty() && existed {
                    self.st.inodes[i].emptied();
                }
                Target::Inode(i)
            }
            (None, Some(d)) => d,
            (None, None) => panic!("an open of nothing"),
        };
        self.st.handles.push(Handle {
            file: f,
            target,
            flags,
            state: HState::Open,
            poisoned: false,
            poisoned_at: 0,
        });
    }

    /// A close's event: the handle goes, and is stale from now on.
    pub fn closed(&mut self, f: Option<longshore_file::File>, result: Result<u64, FileError>) {
        assert_eq!(result, Ok(0), "close");
        let f = f.unwrap();
        let i = self.st.handles.iter().position(|h| h.file == f).unwrap();
        let _gone: Handle = self.st.handles.swap_remove(i);
        self.st.last_gen[f.slot() as usize] = Some(f.generation());
        self.st.stale.push(f);
    }
}
