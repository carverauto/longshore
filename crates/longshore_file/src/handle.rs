// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Handles: a file and an operation, each a slot and a generation, so that
//! one kept past its slot's reuse is refused (contract C3, C4).

/// An open file of a [`crate::Files`] set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct File {
    pub(crate) slot: u32,
    pub(crate) generation: u32,
}

impl File {
    /// The file slot.
    pub const fn slot(self) -> u32 {
        self.slot
    }

    /// The slot's generation when the file was opened.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// An accepted operation, named again by its event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ticket {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

impl Ticket {
    /// The operation slot.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The slot's generation when the operation was accepted.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}
