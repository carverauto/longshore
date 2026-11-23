// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What an operation asks of the store.

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Kind {
    Store {
        no_clobber: bool,
    },
    Fetch {
        range: Option<(u64, u64)>,
    },
    Erase,
    List,
    /// Begin a multipart upload, asking for whole-object checksums if set.
    Begin {
        checksums: bool,
    },
    /// Store one part; its number is in the slot's query.
    Part,
    Complete,
    Abort,
    /// Ask about an object without fetching it.
    Stat,
    /// Copy inside the store; the source is in the slot's query field.
    Copy,
    /// Erase the keys named in the body.
    EraseMany,
}

impl Kind {
    /// The request carries the caller's bytes as an object or part of one.
    pub(super) fn stores_bytes(self) -> bool {
        matches!(self, Kind::Store { .. } | Kind::Part)
    }
}
